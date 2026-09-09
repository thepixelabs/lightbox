// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// Build script for lightbox-rawproxy (E02 Phase C).
//
// LibRaw (LGPL-2.1) is compiled/linked ONLY under the `libraw` cargo feature.
// The DEFAULT build takes the early-return path below and links nothing native,
// so `cargo build --workspace` stays green on machines without LibRaw. Enabling
// `--features libraw` compiles the C shim (`src/libraw_shim.c`) and dynamically
// links `libraw`, the isolation contract: LibRaw is dynamic-linked into this
// sandbox binary only, never statically into an app binary (surface-2 SBOM,
// task C7).

fn main() {
    println!("cargo:rerun-if-changed=src/libraw_shim.c");
    println!("cargo:rerun-if-env-changed=LIBRAW_DIR");

    // Feature-gate: only touch LibRaw when explicitly asked.
    if std::env::var_os("CARGO_FEATURE_LIBRAW").is_none() {
        return;
    }

    // Locate LibRaw: env override → pkg-config → Homebrew default.
    let (include_dir, lib_dir) = locate_libraw();

    let mut build = cc::Build::new();
    build.file("src/libraw_shim.c");
    if let Some(inc) = &include_dir {
        build.include(inc);
    }
    // The shim is C; keep it warnings-clean but not -Werror (LibraW headers
    // are outside our control).
    build.warnings(false);
    build.compile("lbx_libraw_shim");

    if let Some(dir) = &lib_dir {
        println!("cargo:rustc-link-search=native={dir}");
    }
    // Dynamic link (LGPL-2.1 relink requirement, never a static app link).
    println!("cargo:rustc-link-lib=dylib=raw");
}

/// Returns `(include_dir, lib_dir)` for LibRaw, panicking with an actionable
/// message when the `libraw` feature is on but LibRaw cannot be found. Panicking
/// in a build script is the sanctioned way to fail a feature build; the DEFAULT
/// build never reaches here.
fn locate_libraw() -> (Option<String>, Option<String>) {
    // 1. Explicit override: LIBRAW_DIR points at a prefix with include/ + lib/.
    if let Some(dir) = std::env::var_os("LIBRAW_DIR") {
        let dir = dir.to_string_lossy().into_owned();
        return (Some(format!("{dir}/include")), Some(format!("{dir}/lib")));
    }

    // 2. pkg-config (Linux distros, MacPorts, vcpkg-with-pc).
    if let Ok(out) = std::process::Command::new("pkg-config")
        .args(["--variable=prefix", "libraw"])
        .output()
    {
        if out.status.success() {
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !prefix.is_empty() {
                return (
                    Some(format!("{prefix}/include")),
                    Some(format!("{prefix}/lib")),
                );
            }
        }
    }

    // 3. Homebrew default (the build-machine reality per E02 §0).
    for prefix in ["/opt/homebrew/opt/libraw", "/usr/local/opt/libraw"] {
        if std::path::Path::new(&format!("{prefix}/include/libraw/libraw.h")).exists() {
            return (
                Some(format!("{prefix}/include")),
                Some(format!("{prefix}/lib")),
            );
        }
    }

    panic!(
        "lightbox-rawproxy: the `libraw` feature is enabled but LibRaw was not found.\n\
         Install it (`brew install libraw`) or set LIBRAW_DIR to its prefix.\n\
         The default build (without --features libraw) does not require LibRaw."
    );
}
