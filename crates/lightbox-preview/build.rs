// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Feature-gated (`jxl`, T11) libjxl discovery + `bindgen`.
//!
//! A no-op unless built with `--features jxl`, the default build never
//! needs libjxl, its headers, or a C compiler for this crate (spec Risk
//! R1's reversal trigger: the `jxl` feature is OFF by default, and this
//! build machine has no libjxl installed at all, confirmed absent via
//! `brew`/`pkg-config`/binary search before writing this file, see
//! `docs/plan/epics/E03-deviations.md`, Phase C T11).
//!
//! When the feature IS enabled, this generates the encoder FFI surface at
//! build time from whatever `jxl/encode.h` is actually installed on the
//! building machine, deliberately NOT a hand-transcribed struct layout:
//! `JxlBasicInfo` is a large, version-sensitive C struct, and guessing its
//! field layout from memory (rather than letting `bindgen` introspect the
//! real, present header) would be a silent ABI/memory-safety bug waiting to
//! happen. **This path is UNVERIFIED in this repository's CI/dev
//! environment**, there is no machine in this session with libjxl
//! installed to build+link against, so `cargo build --features jxl` has
//! never actually succeeded end-to-end here. It is written as carefully as
//! the spec's own stable, documented C API allows (modeled on libjxl's own
//! `examples/encode_oneshot.c`), gated so a mistake here can never affect
//! the default build, and flagged honestly in the deviations log.

fn main() {
    println!("cargo:rerun-if-env-changed=LIBJXL_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=LIBJXL_LIB_DIR");
    println!("cargo:rerun-if-changed=src/codec/jxl_wrapper.h");

    #[cfg(feature = "jxl")]
    jxl_ffi::build();
}

#[cfg(feature = "jxl")]
mod jxl_ffi {
    use std::env;
    use std::path::Path;

    pub fn build() {
        let include_dir = env::var("LIBJXL_INCLUDE_DIR").ok();
        let lib_dir = env::var("LIBJXL_LIB_DIR").ok();

        if let Some(dir) = &lib_dir {
            println!("cargo:rustc-link-search=native={dir}");
        }
        // Static linkage per spec §2/R1 ("own minimal libjxl encode FFI...
        // BSD-3, static"). The exact set of archive names a libjxl static
        // build produces has varied across releases (a bare `jxl`, plus a
        // split-out `jxl_cms` color-management module since ~0.9, plus its
        // vendored `highway`/`brotli` in some build configurations), this
        // list is a best-effort, UNVERIFIED default; override by adjusting
        // this file once a real libjxl build is available to link against.
        for lib in [
            "jxl",
            "jxl_cms",
            "jxl_threads",
            "hwy",
            "brotlienc",
            "brotlidec",
            "brotlicommon",
        ] {
            println!("cargo:rustc-link-lib=static={lib}");
        }

        let wrapper = Path::new("src/codec/jxl_wrapper.h");
        let mut builder = bindgen::Builder::default()
            .header(wrapper.to_string_lossy().into_owned())
            .allowlist_function("JxlEncoder.*")
            .allowlist_function("JxlColorEncoding.*")
            .allowlist_type("Jxl.*")
            .allowlist_var("JXL_.*")
            .default_enum_style(bindgen::EnumVariation::Rust {
                non_exhaustive: false,
            })
            .generate_comments(false);

        if let Some(dir) = &include_dir {
            builder = builder.clang_arg(format!("-I{dir}"));
        }

        let bindings = builder.generate().unwrap_or_else(|e| {
            panic!(
                "bindgen failed to generate libjxl encoder bindings ({e}) — \
                 is LIBJXL_INCLUDE_DIR set to a directory containing jxl/encode.h? \
                 (the `jxl` feature requires a real libjxl install; see \
                 crates/lightbox-preview/src/codec/jxl.rs's module doc comment)"
            )
        });

        let out_dir = env::var("OUT_DIR").expect("OUT_DIR is always set for build scripts");
        bindings
            .write_to_file(Path::new(&out_dir).join("jxl_bindings.rs"))
            .expect("failed to write generated libjxl bindings to OUT_DIR");
    }
}
