// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Build-time WGSL validation (E05 task **A6**, A-gpu).
//!
//! Every shader under `shaders/` is parsed and validated by **naga** at build
//! time, so **invalid WGSL fails the build, not runtime** (the A6 acceptance
//! criterion). naga is wgpu's own shader front-end, already vendored in the
//! graph at the pinned wgpu version, so this adds no new external surface (see
//! `E05-deviations.md`, A-gpu D-A6).

use std::path::Path;

fn main() {
    let dir = Path::new("shaders");
    println!("cargo:rerun-if-changed=shaders");

    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => panic!("cannot read shaders/ directory: {err}"),
    };

    let mut count = 0usize;
    for entry in entries {
        let path = entry.expect("read shaders/ entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("wgsl") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        let module = match naga::front::wgsl::parse_str(&src) {
            Ok(module) => module,
            Err(err) => panic!(
                "WGSL parse error in {}:\n{}",
                path.display(),
                err.emit_to_string(&src)
            ),
        };
        if let Err(err) = validator.validate(&module) {
            panic!(
                "WGSL validation error in {}:\n{}",
                path.display(),
                err.emit_to_string(&src)
            );
        }
        count += 1;
    }
    assert!(
        count > 0,
        "no WGSL shaders found under shaders/ to validate"
    );
}
