// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Headless `GpuContext` test, E01 spec §5 T5 acceptance criteria: the
//! headless path obtains a device on every CI OS (software adapters count),
//! and a no-adapter environment degrades to `None` without panicking.
//!
//! Mirrors the adapter smoke test's escape hatch: a machine with genuinely no
//! adapter opts out via `LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER=1`; CI never sets it.

#[test]
fn headless_gpu_context_is_available_or_degrades_gracefully() {
    // Must never panic, adapter or not (T5: "no-adapter environment degrades
    // to None without panic").
    match lightbox_render::GpuContext::headless() {
        Some(gpu) => {
            println!("headless GpuContext: {}", gpu.adapter_report());
            assert!(gpu.limits.max_texture_dimension_2d >= 2048);
        }
        None => {
            if std::env::var_os("LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER").is_some() {
                eprintln!(
                    "no wgpu adapter; tolerated because LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER is set"
                );
            } else {
                panic!(
                    "GpuContext::headless() found no adapter. CI runners must provide one \
                     (Metal / DX12 WARP / Vulkan lavapipe). Set \
                     LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER=1 to skip on genuinely headless machines."
                );
            }
        }
    }
}
