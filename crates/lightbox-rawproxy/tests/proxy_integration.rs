// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// Supervisor ⇄ proxy integration (E02 Phase C, tasks C1/C3). Drives the REAL
// spawned `lightbox-rawproxy` binary through `lightbox_decode::ProxySupervisor`.
// `env!("CARGO_BIN_EXE_lightbox-rawproxy")` is provided to a bin crate's
// integration tests, which is why the supervisor's crash/timeout tests live
// here rather than inside `lightbox-decode`.
//
// These run against the DEFAULT (no-`libraw`) proxy: real decode needs a raw
// corpus (A2, deferred), so decode requests return the structured `no_libraw`
// error, which is exactly what proves the protocol, warm-pool, and
// crash/timeout supervision independent of LibRaw. The `libraw`-gated real
// decode test is env-gated below.

use std::path::Path;
use std::time::Duration;

use lightbox_decode::{DecodeError, ProxyClient, ProxyConfig, ProxySupervisor};

fn proxy_bin() -> &'static str {
    env!("CARGO_BIN_EXE_lightbox-rawproxy")
}

fn plain_config() -> ProxyConfig {
    ProxyConfig::new(proxy_bin())
}

fn faults_config() -> ProxyConfig {
    let mut c = ProxyConfig::new(proxy_bin());
    c.env
        .push(("LIGHTBOX_RAWPROXY_TEST_MODE".into(), "faults".into()));
    c
}

#[test]
fn handshake_completes_and_reports_a_version() {
    let client = ProxyClient::spawn(&plain_config()).expect("spawn + handshake");
    let v = client
        .libraw_version()
        .expect("version reported at handshake");
    assert!(!v.is_empty(), "libraw version string is non-empty");
}

#[test]
fn warm_child_serves_multiple_requests_without_dying() {
    let sup = ProxySupervisor::new(plain_config());
    for _ in 0..5 {
        let r = sup.decode_mosaic(
            Path::new("/tmp/does-not-exist.raw"),
            Duration::from_secs(10),
        );
        // Default build: structured `no_libraw` -> Unimplemented. Never a crash.
        assert!(r.is_err(), "expected a structured error");
        assert!(
            !matches!(r, Err(DecodeError::ProxyCrashed { .. })),
            "warm child must not die: {r:?}"
        );
    }
    // The same warm child is still answering (its version is cached).
    assert!(sup.libraw_version().is_some());
}

#[test]
fn injected_crash_is_reported_and_a_fresh_child_serves_the_next_request() {
    let sup = ProxySupervisor::new(faults_config());
    // A path carrying the crash marker makes the child abort mid-request.
    let crash = sup.decode_mosaic(Path::new("/tmp/__LBX_CRASH__.raw"), Duration::from_secs(10));
    assert!(
        matches!(crash, Err(DecodeError::ProxyCrashed { .. })),
        "child crash must surface as ProxyCrashed, got {crash:?}"
    );
    // Next request spawns a fresh child that answers normally (no marker).
    let next = sup.decode_mosaic(Path::new("/tmp/ordinary.raw"), Duration::from_secs(10));
    assert!(next.is_err(), "structured error expected");
    assert!(
        !matches!(next, Err(DecodeError::ProxyCrashed { .. })),
        "the fresh child must serve the next request: {next:?}"
    );
}

#[test]
fn injected_hang_times_out_and_recovers_on_a_fresh_child() {
    let sup = ProxySupervisor::new(faults_config());
    let hang = sup.decode_mosaic(
        Path::new("/tmp/__LBX_HANG__.raw"),
        Duration::from_millis(400),
    );
    assert!(
        matches!(hang, Err(DecodeError::ProxyTimeout)),
        "a hung child must be killed and surface ProxyTimeout, got {hang:?}"
    );
    // A fresh child handles the next request within budget (no timeout).
    let next = sup.decode_mosaic(Path::new("/tmp/ordinary.raw"), Duration::from_secs(10));
    assert!(
        !matches!(next, Err(DecodeError::ProxyTimeout)),
        "the fresh child must not time out: {next:?}"
    );
}

#[test]
fn demosaiced_request_round_trips_the_protocol() {
    // Exercises the DecodeDemosaiced request path end-to-end over the wire.
    let sup = ProxySupervisor::new(plain_config());
    let out = sup.decode_demosaiced(
        Path::new("/tmp/does-not-exist.raw"),
        &lightbox_decode::LibrawParams::default(),
        Duration::from_secs(10),
    );
    assert!(out.is_err());
    assert!(!matches!(out, Err(DecodeError::ProxyCrashed { .. })));
}

/// License/isolation audit (task C7): the DEFAULT proxy binary, the one that
/// ships unless packaging opts into `--features libraw`, must contain **no**
/// LibRaw symbols. Since only the feature build links LibRaw, and the app never
/// links the proxy (it spawns it), a clean default proxy proves the app binaries
/// are LibRaw-free too. Best-effort: skips if `nm` is unavailable.
#[cfg(not(feature = "libraw"))]
#[test]
fn no_libraw_symbols_in_default_proxy_binary() {
    let out = match std::process::Command::new("nm").arg(proxy_bin()).output() {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skipping: `nm` unavailable");
            return;
        }
    };
    // `nm` exits non-zero on a stripped binary with no symbols, that is itself
    // a pass (no LibRaw symbols). Only inspect stdout when present.
    //
    // Match ACTUAL LibRaw entry points + our shim's exported functions, not the
    // bare substring "libraw" (our own `no_libraw` fallback / `NO_LIBRAW` code
    // legitimately contain it as plain data/identifiers).
    let symbols = String::from_utf8_lossy(&out.stdout).to_lowercase();
    for needle in [
        "libraw_init",
        "libraw_unpack",
        "libraw_open_file",
        "libraw_dcraw_process",
        "libraw_version",
        "lbx_lr_open_unpack",
        "lbx_lr_make_mem_image",
        "6libraw", // mangled C++ `LibRaw` class
    ] {
        assert!(
            !symbols.contains(needle),
            "default proxy binary must carry no `{needle}` symbol (LibRaw isolation, C7)"
        );
    }
}

/// Real LibRaw decode, only when built with `--features libraw` AND
/// `LIGHTBOX_TEST_RAW` names a readable raw file. Skips otherwise (honest: no
/// corpus is fabricated; A2 is deferred).
#[cfg(feature = "libraw")]
#[test]
fn real_raw_decodes_via_the_proxy_when_a_sample_is_provided() {
    let Some(raw) = std::env::var_os("LIGHTBOX_TEST_RAW") else {
        eprintln!("skipping: set LIGHTBOX_TEST_RAW to a raw file to exercise real decode");
        return;
    };
    let raw = std::path::PathBuf::from(raw);
    if !raw.exists() {
        eprintln!(
            "skipping: LIGHTBOX_TEST_RAW does not exist: {}",
            raw.display()
        );
        return;
    }
    let sup = ProxySupervisor::new(plain_config());

    let mosaic = sup
        .decode_mosaic(&raw, Duration::from_secs(60))
        .expect("mosaic decode");
    assert!(mosaic.width > 0 && mosaic.height > 0);
    assert_eq!(
        mosaic.data.samples.len(),
        (mosaic.width as usize) * (mosaic.height as usize)
    );

    let src = sup
        .decode_demosaiced(
            &raw,
            &lightbox_decode::LibrawParams::default(),
            Duration::from_secs(60),
        )
        .expect("demosaiced decode");
    assert!(src.provenance.interim_demosaic);
    assert_eq!(src.channels, 3);
    assert_eq!(
        src.data.len(),
        (src.width as usize) * (src.height as usize) * 3
    );
    assert!(matches!(
        src.color,
        lightbox_decode::SourceColor::CameraNative(_)
    ));
    // Version came from the real linked LibRaw.
    assert!(sup.libraw_version().is_some());
}
