// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0
//
// FFI crate: LibRaw is dynamic-linked here in Phase C (LGPL-2.1, isolated
// out-of-process, surface-2 SBOM). `unsafe` is allowed crate-locally with that
// justification; Phase A ships no unsafe yet.
#![allow(unsafe_code)]

//! `lightbox-rawproxy` — the out-of-process LibRaw decode sandbox (spec §3.2 /
//! §5.1, E02 Phase C). Speaks the v1 wire protocol (u32-LE length-prefixed CBOR
//! frames over stdin/stdout, pixel payloads out-of-band via shm) with the
//! supervisor client in `lightbox-decode`.
//!
//! **Scaffold state (E02 Phase A):** the frame loop + `Hello`/`Shutdown`
//! handshake are real so C1's protocol round-trip has a foundation; the LibRaw
//! FFI, the shm payload handoff, and the rlimit/JobObject sandbox are Phase C
//! (C2–C4). Decode requests currently answer a structured `Err` frame — the
//! parent maps it to a `DecodeError`, never a broken pipe.

use std::io::{self, Read, Write};

use lightbox_decode::{ProxyRequest, ProxyResponse, PROTO_VERSION};

/// Hard cap on an inbound request frame (a request is metadata only; pixel
/// payloads flow the other way, out-of-band). Rejecting oversize frames is the
/// child-side half of the §3.2 payload caps.
const MAX_REQUEST_FRAME: u32 = 1 << 20; // 1 MiB

/// Reads one length-prefixed frame. `Ok(None)` on a clean EOF between frames.
fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_REQUEST_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("request frame {len} exceeds cap {MAX_REQUEST_FRAME}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Writes one length-prefixed frame.
fn write_frame(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response frame too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(bytes)?;
    w.flush()
}

/// Handles one decoded request. Returns `None` to signal graceful shutdown.
fn handle(req: ProxyRequest) -> Option<ProxyResponse> {
    match req {
        ProxyRequest::Hello { .. } => Some(ProxyResponse::Hello {
            proto: PROTO_VERSION,
            // Phase C reports the real linked LibRaw version.
            libraw_version: "absent (Phase A scaffold)".to_owned(),
        }),
        ProxyRequest::Shutdown => None,
        ProxyRequest::Probe { .. }
        | ProxyRequest::DecodeMosaic { .. }
        | ProxyRequest::DecodeDemosaiced { .. } => Some(ProxyResponse::Err {
            code: "unimplemented".to_owned(),
            detail: "LibRaw decode lands in E02 Phase C (C2)".to_owned(),
        }),
    }
}

fn run() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    while let Some(frame) = read_frame(&mut reader)? {
        let req: ProxyRequest = match ciborium::from_reader(frame.as_slice()) {
            Ok(req) => req,
            Err(e) => {
                let mut out = Vec::new();
                let resp = ProxyResponse::Err {
                    code: "bad_frame".to_owned(),
                    detail: format!("undecodable request frame: {e}"),
                };
                // A serialization failure here is unrecoverable; propagate.
                ciborium::into_writer(&resp, &mut out)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                write_frame(&mut writer, &out)?;
                continue;
            }
        };

        let Some(resp) = handle(req) else {
            break; // Shutdown.
        };
        let mut out = Vec::new();
        ciborium::into_writer(&resp, &mut out).map_err(|e| io::Error::other(e.to_string()))?;
        write_frame(&mut writer, &out)?;
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("lightbox-rawproxy: {e}");
        std::process::exit(1);
    }
}
