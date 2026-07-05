// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Process hardening (E02 Phase C, task C4). Applied once at proxy startup,
//! before any file is opened, so the memory-unsafe LibRaw decoder runs inside
//! bounded resource limits.
//!
//! **POSIX (implemented):** address-space, CPU-time, core-dump, and file-size
//! rlimits via `setrlimit(2)` ([`crate::limits`] holds the numbers). These cap
//! the blast radius of a decode bomb even if the parent's watchdog is delayed.
//!
//! **Windows (best-effort, documented):** the equivalent is a Job Object with
//! `JOB_OBJECT_LIMIT_PROCESS_MEMORY` + `JOB_OBJECT_LIMIT_JOB_TIME`, applied by
//! the *parent* to the spawned child (a child cannot place itself in a killable
//! job as cleanly). The build machine is macOS; the Windows Job Object wiring is
//! a documented follow-up (E02-deviations.md) and the parent-side payload cap +
//! kill-on-timeout supervisor already bound the child there in the interim.
//!
//! **No-network:** the proxy opens no sockets — it reads the input file, writes
//! a temp payload, and speaks stdio. Hard egress prevention (seccomp /
//! seatbelt / AppContainer) is scoped to the §12 security-engineer review; the
//! threat-model notes (task H7) record it as an open item. Nothing here claims a
//! network sandbox that does not exist.

/// Applies the child-side resource limits. Never fails the process — a limit
/// that cannot be set is logged to stderr and skipped (the supervisor's
/// kill-on-timeout + parent payload cap remain the backstop).
pub fn apply() {
    #[cfg(unix)]
    apply_posix();
    #[cfg(not(unix))]
    {
        // Windows/other: parent-applied Job Object is the intended mechanism
        // (see the module note); nothing to do child-side.
    }
}

#[cfg(unix)]
fn apply_posix() {
    use crate::limits;

    // A macro (not a fn) so the resource constant keeps its platform-native
    // type — `c_int` on macOS/BSD, `__rlimit_resource_t` on glibc — without a
    // typed function parameter that would only compile on one of them.
    macro_rules! set_rlimit {
        ($res:expr, $limit:expr, $name:expr) => {{
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: `current` is a valid, writable rlimit out-param.
            let hard = if unsafe { libc::getrlimit($res, &mut current) } == 0 {
                current.rlim_max
            } else {
                libc::RLIM_INFINITY
            };
            // Only ever *lower* a limit (raising above the hard cap is EPERM).
            let want: libc::rlim_t = if hard == libc::RLIM_INFINITY {
                $limit as libc::rlim_t
            } else {
                ($limit as libc::rlim_t).min(hard)
            };
            let rl = libc::rlimit {
                rlim_cur: want,
                rlim_max: hard,
            };
            // SAFETY: `rl` is a valid rlimit; `$res` is a valid resource const.
            if unsafe { libc::setrlimit($res, &rl) } != 0 {
                eprintln!(
                    "lightbox-rawproxy: could not set {} ({})",
                    $name,
                    std::io::Error::last_os_error()
                );
            }
        }};
    }

    // Address-space cap: enforced on Linux/BSD via RLIMIT_AS. macOS rejects
    // both RLIMIT_AS and RLIMIT_DATA at these sizes (setrlimit → EINVAL) and
    // does not meaningfully enforce a virtual-memory cap via setrlimit, so we
    // skip it there and rely on RLIMIT_CPU + the parent's kill-on-timeout
    // watchdog and payload cap (documented, not silently claimed).
    #[cfg(not(target_os = "macos"))]
    set_rlimit!(
        libc::RLIMIT_AS,
        limits::RLIMIT_ADDRESS_SPACE_BYTES,
        "RLIMIT_AS"
    );
    set_rlimit!(libc::RLIMIT_CPU, limits::RLIMIT_CPU_SECONDS, "RLIMIT_CPU");
    // No core dumps (a crash of untrusted-input code must not spill memory).
    set_rlimit!(libc::RLIMIT_CORE, 0u64, "RLIMIT_CORE");
    // File size: payloads plus headroom; blocks a runaway from filling disk.
    set_rlimit!(
        libc::RLIMIT_FSIZE,
        limits::MAX_PAYLOAD_BYTES + (256 * 1024 * 1024),
        "RLIMIT_FSIZE"
    );
}
