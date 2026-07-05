// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Supervisor client for the out-of-process LibRaw proxy (E02 Phase C, task C3
//! — the primary CFA-mosaic decode path).
//!
//! [`ProxySupervisor`] keeps **one warm pooled child** ([`ProxyClient`]) and,
//! per spec §3.2 / §5.1, guarantees the caller sees a structured
//! [`DecodeError`] and never a broken pipe:
//!
//! - **Timeout** — each request has a budget; a child that overruns it is
//!   killed and the call returns [`DecodeError::ProxyTimeout`]. The next call
//!   spawns a fresh child.
//! - **Crash** — if the child dies (SIGKILL / abort / broken pipe) the call
//!   returns [`DecodeError::ProxyCrashed`]; the next call spawns a fresh child.
//! - **No in-process retry** — there is exactly one mosaic backend (rawler is
//!   license-banned, R1); a failed mosaic decode is a per-file
//!   `asset.decode_error`, the asset stays catalogued. The supervisor does not
//!   silently re-run the *same* request on the fresh child.
//!
//! The child stdout is drained by a dedicated reader thread feeding an mpsc
//! channel, so the request path can wait with [`std::sync::mpsc::Receiver::recv_timeout`]
//! without blocking uninterruptibly on a hung child.

use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use lightbox_types::Orientation;

use crate::error::{CapKind, DecodeError};
use crate::raw::proxy::{
    err_code, LibrawParams, ProxyMeta, ProxyPayloadKind, ProxyRequest, ProxyResponse, ShmRef,
    PROTO_VERSION,
};
use crate::raw::state::decode_params_hash;
use crate::raw::types::{
    BackendPolicy, DecodeBackend, MosaicBuffer, MosaicImage, RawColorimetry, SourceColor,
    SourceImage, SourceProvenance,
};

/// Cap on an inbound response frame (metadata only — pixels flow out-of-band).
const MAX_RESPONSE_FRAME: u32 = 16 * 1024 * 1024;

/// Parent-side cap on an out-of-band payload (mirrors the child-side cap).
const MAX_PAYLOAD_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Budget for the `Hello` handshake at spawn time (spec §7.5: cold spawn +
/// handshake ≤ 150 ms; this is a generous ceiling to avoid CI flakiness).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How to launch the proxy binary.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    /// Path to the `lightbox-rawproxy` executable.
    pub binary: PathBuf,
    /// Extra environment for the child (used by tests for fault injection).
    pub env: Vec<(String, String)>,
}

impl ProxyConfig {
    /// A config launching `binary` with no extra environment.
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        ProxyConfig {
            binary: binary.into(),
            env: Vec::new(),
        }
    }

    /// Locates the proxy binary: `LIGHTBOX_RAWPROXY_BIN` if set, else a sibling
    /// of the current executable (how the app ships it). Returns `None` when
    /// neither exists.
    pub fn autodetect() -> Option<Self> {
        if let Some(p) = std::env::var_os("LIGHTBOX_RAWPROXY_BIN") {
            let p = PathBuf::from(p);
            if p.exists() {
                return Some(ProxyConfig::new(p));
            }
        }
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let name = if cfg!(windows) {
            "lightbox-rawproxy.exe"
        } else {
            "lightbox-rawproxy"
        };
        let candidate = dir.join(name);
        if candidate.exists() {
            Some(ProxyConfig::new(candidate))
        } else {
            None
        }
    }
}

/// One warm proxy child and the reader thread draining its stdout.
#[derive(Debug)]
pub struct ProxyClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<ReaderMsg>,
    reader: Option<JoinHandle<()>>,
    /// LibRaw version the child reported at handshake (recorded in provenance).
    libraw_version: Option<String>,
    /// Cleared once the child is known dead so a stale handle is never reused.
    alive: bool,
}

/// What the reader thread hands back per frame.
// `Response` carries a full `ProxyResponse` (large `Ok` variant); this is an
// ephemeral channel message, so boxing buys nothing over the allow.
#[allow(clippy::large_enum_variant)]
enum ReaderMsg {
    /// A decoded response frame.
    Response(ProxyResponse),
    /// The stream ended or a frame was undecodable (child is effectively gone).
    Closed,
}

impl ProxyClient {
    /// Spawns a child and completes the version handshake. On any spawn or
    /// handshake failure returns a [`DecodeError`] (never panics).
    pub fn spawn(config: &ProxyConfig) -> Result<ProxyClient, DecodeError> {
        let mut cmd = Command::new(&config.binary);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in &config.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("proxy child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("proxy child has no stdout"))?;

        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                match read_frame(&mut r) {
                    Ok(Some(bytes)) => {
                        match ciborium::from_reader::<ProxyResponse, _>(&bytes[..]) {
                            Ok(resp) => {
                                if tx.send(ReaderMsg::Response(resp)).is_err() {
                                    break; // client dropped
                                }
                            }
                            Err(_) => {
                                let _ = tx.send(ReaderMsg::Closed);
                                break;
                            }
                        }
                    }
                    // Clean EOF or a read error both mean the child is gone.
                    Ok(None) | Err(_) => {
                        let _ = tx.send(ReaderMsg::Closed);
                        break;
                    }
                }
            }
        });

        let mut client = ProxyClient {
            child,
            stdin,
            rx,
            reader: Some(reader),
            libraw_version: None,
            alive: true,
        };

        // Handshake.
        let resp = client.request(
            ProxyRequest::Hello {
                proto: PROTO_VERSION,
            },
            HANDSHAKE_TIMEOUT,
        )?;
        match resp {
            ProxyResponse::Hello {
                proto,
                libraw_version,
            } => {
                if proto != PROTO_VERSION {
                    client.alive = false;
                    return Err(DecodeError::CorruptFile {
                        detail: format!(
                            "proxy protocol mismatch: client v{PROTO_VERSION}, server v{proto}"
                        ),
                    });
                }
                client.libraw_version = Some(libraw_version);
                Ok(client)
            }
            other => {
                client.alive = false;
                Err(DecodeError::CorruptFile {
                    detail: format!("expected Hello from proxy, got {other:?}"),
                })
            }
        }
    }

    /// The LibRaw version the child reported (`None` for a default-build proxy
    /// without the `libraw` feature).
    pub fn libraw_version(&self) -> Option<&str> {
        self.libraw_version.as_deref()
    }

    /// Sends one request and waits up to `timeout` for the reply. A timeout
    /// kills the child; a closed stream is a crash. Either way the client is
    /// marked dead so the supervisor spawns a fresh one next time.
    fn request(
        &mut self,
        req: ProxyRequest,
        timeout: Duration,
    ) -> Result<ProxyResponse, DecodeError> {
        if !self.alive {
            return Err(DecodeError::ProxyCrashed { signal: None });
        }
        if let Err(e) = self.write_request(&req) {
            self.alive = false;
            // A broken pipe means the child died before/while we wrote.
            return Err(if e.kind() == io::ErrorKind::BrokenPipe {
                DecodeError::ProxyCrashed {
                    signal: self.reap_signal(),
                }
            } else {
                DecodeError::Io(e)
            });
        }

        match self.rx.recv_timeout(timeout) {
            Ok(ReaderMsg::Response(resp)) => Ok(resp),
            Ok(ReaderMsg::Closed) => {
                self.alive = false;
                Err(DecodeError::ProxyCrashed {
                    signal: self.reap_signal(),
                })
            }
            Err(RecvTimeoutError::Timeout) => {
                self.alive = false;
                let _ = self.child.kill();
                Err(DecodeError::ProxyTimeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.alive = false;
                Err(DecodeError::ProxyCrashed {
                    signal: self.reap_signal(),
                })
            }
        }
    }

    fn write_request(&mut self, req: &ProxyRequest) -> io::Result<()> {
        let mut buf = Vec::new();
        ciborium::into_writer(req, &mut buf).map_err(|e| io::Error::other(e.to_string()))?;
        write_frame(&mut self.stdin, &buf)
    }

    /// Best-effort terminating-signal read once the child is known dead.
    fn reap_signal(&mut self) -> Option<i32> {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            match self.child.wait() {
                Ok(status) => status.signal(),
                Err(_) => None,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.wait();
            None
        }
    }
}

impl Drop for ProxyClient {
    fn drop(&mut self) {
        // Best-effort graceful shutdown, then ensure the child is gone so the
        // reader thread's blocking read unblocks and the join returns.
        if self.alive {
            let _ = self.write_request(&ProxyRequest::Shutdown);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Keeps a single warm proxy child, (re)spawning it as needed. Thread-safe.
pub struct ProxySupervisor {
    config: ProxyConfig,
    client: Mutex<Option<ProxyClient>>,
}

impl ProxySupervisor {
    /// A supervisor that launches the proxy per `config`. No child is spawned
    /// until the first decode.
    pub fn new(config: ProxyConfig) -> Self {
        ProxySupervisor {
            config,
            client: Mutex::new(None),
        }
    }

    /// Convenience: [`ProxyConfig::autodetect`] then [`ProxySupervisor::new`].
    pub fn autodetect() -> Option<Self> {
        ProxyConfig::autodetect().map(ProxySupervisor::new)
    }

    /// The LibRaw version reported by the current warm child, if any is live.
    pub fn libraw_version(&self) -> Option<String> {
        self.client
            .lock()
            .ok()?
            .as_ref()
            .and_then(|c| c.libraw_version().map(str::to_owned))
    }

    /// Runs `op` against a live warm child, spawning one if needed. On
    /// crash/timeout the dead child is dropped so the next call respawns; there
    /// is no in-process retry of the failed request (spec §5.1).
    fn with_client<T>(
        &self,
        timeout: Duration,
        op: impl FnOnce(&mut ProxyClient, Duration) -> Result<T, DecodeError>,
    ) -> Result<T, DecodeError> {
        let mut slot = self
            .client
            .lock()
            .map_err(|_| io::Error::other("proxy supervisor mutex poisoned"))?;
        if slot.is_none() || !slot.as_ref().map(|c| c.alive).unwrap_or(false) {
            *slot = Some(ProxyClient::spawn(&self.config)?);
        }
        let client = slot.as_mut().expect("just ensured Some");
        let result = op(client, timeout);
        if matches!(
            result,
            Err(DecodeError::ProxyCrashed { .. }) | Err(DecodeError::ProxyTimeout)
        ) {
            // Drop the dead child; the next call spawns fresh.
            *slot = None;
        }
        result
    }

    /// Decodes a CFA-mosaic raw via the proxy (primary path). The `u16` plane
    /// arrives out-of-band; this rebuilds the [`MosaicImage`].
    pub fn decode_mosaic(
        &self,
        path: &Path,
        timeout: Duration,
    ) -> Result<MosaicImage, DecodeError> {
        let path = path.to_path_buf();
        self.with_client(timeout, move |client, timeout| {
            let resp = client.request(ProxyRequest::DecodeMosaic { path }, timeout)?;
            let (meta, shm) = expect_ok(resp)?;
            let bytes = read_payload(&shm)?;
            let mm = match meta.payload {
                ProxyPayloadKind::Mosaic(mm) => mm,
                other => {
                    return Err(DecodeError::CorruptFile {
                        detail: format!("proxy returned a non-mosaic payload: {other:?}"),
                    })
                }
            };
            let samples = le_bytes_to_u16(&bytes);
            let expected = (meta.width as usize) * (meta.height as usize);
            if samples.len() != expected {
                return Err(DecodeError::CorruptFile {
                    detail: format!(
                        "mosaic payload {} samples != expected {expected}",
                        samples.len()
                    ),
                });
            }
            Ok(MosaicImage {
                data: MosaicBuffer { samples },
                width: meta.width,
                height: meta.height,
                cfa: mm.cfa,
                active_area: mm.active_area,
                default_crop: mm.default_crop,
                black_levels: mm.black_levels,
                white_levels: mm.white_levels,
                linearization: mm.linearization,
                colorimetry: mm.colorimetry,
                orientation: Orientation::from_exif(mm.orientation_exif).unwrap_or_default(),
            })
        })
    }

    /// Decodes the interim demosaiced (AHD) path via the proxy: linear
    /// camera-native RGB, no WB / output color / gamma. Returns a
    /// [`SourceImage`] with [`SourceColor::CameraNative`] and provenance flags
    /// (`interim_demosaic = true`).
    pub fn decode_demosaiced(
        &self,
        path: &Path,
        params: &LibrawParams,
        timeout: Duration,
    ) -> Result<SourceImage, DecodeError> {
        let path = path.to_path_buf();
        let params = params.clone();
        self.with_client(timeout, move |client, timeout| {
            let resp = client.request(ProxyRequest::DecodeDemosaiced { path, params }, timeout)?;
            let (meta, shm) = expect_ok(resp)?;
            let bytes = read_payload(&shm)?;
            let dm = match &meta.payload {
                ProxyPayloadKind::Demosaiced(dm) => dm.clone(),
                other => {
                    return Err(DecodeError::CorruptFile {
                        detail: format!("proxy returned a non-demosaiced payload: {other:?}"),
                    })
                }
            };
            let samples = le_bytes_to_u16(&bytes);
            let channels = dm.channels.max(1) as usize;
            let expected = (meta.width as usize) * (meta.height as usize) * channels;
            if samples.len() != expected {
                return Err(DecodeError::CorruptFile {
                    detail: format!(
                        "demosaiced payload {} samples != expected {expected}",
                        samples.len()
                    ),
                });
            }
            let scale = 1.0f32 / (dm.max_value.max(1) as f32);
            let data: Vec<f32> = samples.iter().map(|&s| f32::from(s) * scale).collect();
            let provenance = SourceProvenance {
                backend: DecodeBackend::LibrawProxy,
                interim_demosaic: true,
                decode_params_hash: decode_params_hash(
                    BackendPolicy::Auto,
                    true,
                    meta.libraw_version.as_deref(),
                ),
            };
            Ok(SourceImage {
                data,
                width: meta.width,
                height: meta.height,
                channels: dm.channels,
                color: SourceColor::CameraNative(carry_colorimetry(dm.colorimetry)),
                provenance,
            })
        })
    }
}

/// Unwraps a successful reply into `(meta, ShmRef)`, mapping an `Err` frame to a
/// [`DecodeError`] and any other frame to a protocol error.
fn expect_ok(resp: ProxyResponse) -> Result<(ProxyMeta, ShmRef), DecodeError> {
    match resp {
        ProxyResponse::Ok {
            meta,
            payload: Some(shm),
        } => Ok((meta, shm)),
        ProxyResponse::Ok { payload: None, .. } => Err(DecodeError::CorruptFile {
            detail: "proxy returned Ok with no pixel payload".to_owned(),
        }),
        ProxyResponse::Err { code, detail } => Err(map_err_code(&code, detail)),
        other => Err(DecodeError::CorruptFile {
            detail: format!("unexpected proxy reply: {other:?}"),
        }),
    }
}

/// Maps a proxy `Err` code to a [`DecodeError`].
fn map_err_code(code: &str, detail: String) -> DecodeError {
    match code {
        err_code::NO_LIBRAW => {
            DecodeError::Unimplemented("rawproxy built without the `libraw` feature")
        }
        err_code::RESOURCE_CAP_DIMENSIONS => DecodeError::ResourceCap {
            cap: CapKind::Dimensions,
        },
        err_code::RESOURCE_CAP_PAYLOAD => DecodeError::ResourceCap {
            cap: CapKind::Payload,
        },
        // open/unpack failure, bad frame, proto mismatch, or anything unknown:
        // a per-file structured decode failure.
        _ => DecodeError::CorruptFile {
            detail: format!("{code}: {detail}"),
        },
    }
}

/// Reads (and always deletes) the out-of-band payload file named by `shm`,
/// enforcing the parent-side payload cap.
fn read_payload(shm: &ShmRef) -> Result<Vec<u8>, DecodeError> {
    struct Cleanup<'a>(&'a str);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _cleanup = Cleanup(shm.name.as_str());

    if shm.len > MAX_PAYLOAD_BYTES {
        return Err(DecodeError::ResourceCap {
            cap: CapKind::Payload,
        });
    }
    let bytes = std::fs::read(&shm.name)?;
    Ok(bytes)
}

fn le_bytes_to_u16(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Identity pass-through kept as a seam: any future normalization of the
/// proxy-supplied colorimetry (e.g. filling a derived ForwardMatrix) lands here.
fn carry_colorimetry(c: RawColorimetry) -> RawColorimetry {
    c
}

/// Reads one `u32`-LE length-prefixed frame from `r`. `Ok(None)` on clean EOF.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_RESPONSE_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("response frame {len} exceeds cap {MAX_RESPONSE_FRAME}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Writes one `u32`-LE length-prefixed frame to `w`.
fn write_frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autodetect_env_missing_binary_is_none() {
        // A bogus path must not be accepted.
        std::env::set_var(
            "LIGHTBOX_RAWPROXY_BIN",
            "/nonexistent/lightbox-rawproxy-xyz",
        );
        let cfg = ProxyConfig::autodetect();
        std::env::remove_var("LIGHTBOX_RAWPROXY_BIN");
        // Either None, or (if a sibling exists) it did not pick the bogus path.
        if let Some(cfg) = cfg {
            assert_ne!(
                cfg.binary,
                PathBuf::from("/nonexistent/lightbox-rawproxy-xyz")
            );
        }
    }

    #[test]
    fn spawning_a_missing_binary_is_io_error_not_panic() {
        let cfg = ProxyConfig::new("/nonexistent/lightbox-rawproxy-xyz");
        let out = ProxyClient::spawn(&cfg);
        assert!(matches!(out, Err(DecodeError::Io(_))), "{out:?}");
    }

    #[test]
    fn read_payload_returns_bytes_and_always_deletes_the_file() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lbx-client-test-{}.bin", std::process::id()));
        let data = vec![9u8, 8, 7, 6, 5, 4];
        std::fs::write(&path, &data).unwrap();
        let shm = ShmRef {
            name: path.to_string_lossy().into_owned(),
            len: data.len() as u64,
        };
        let out = read_payload(&shm).unwrap();
        assert_eq!(out, data);
        assert!(!path.exists(), "payload file must be deleted after read");
    }

    #[test]
    fn read_payload_over_cap_is_rejected_and_still_deletes() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lbx-client-cap-{}.bin", std::process::id()));
        std::fs::write(&path, [0u8; 8]).unwrap();
        let shm = ShmRef {
            name: path.to_string_lossy().into_owned(),
            len: MAX_PAYLOAD_BYTES + 1, // declared oversize
        };
        let out = read_payload(&shm);
        assert!(matches!(
            out,
            Err(DecodeError::ResourceCap {
                cap: CapKind::Payload
            })
        ));
        assert!(
            !path.exists(),
            "payload file must be deleted even when capped"
        );
    }

    #[test]
    fn map_err_codes_cover_the_taxonomy() {
        assert!(matches!(
            map_err_code(err_code::NO_LIBRAW, String::new()),
            DecodeError::Unimplemented(_)
        ));
        assert!(matches!(
            map_err_code(err_code::RESOURCE_CAP_DIMENSIONS, String::new()),
            DecodeError::ResourceCap {
                cap: CapKind::Dimensions
            }
        ));
        assert!(matches!(
            map_err_code(err_code::RESOURCE_CAP_PAYLOAD, String::new()),
            DecodeError::ResourceCap {
                cap: CapKind::Payload
            }
        ));
        assert!(matches!(
            map_err_code(err_code::OPEN_FAILED, "boom".to_owned()),
            DecodeError::CorruptFile { .. }
        ));
    }
}
