// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! wgpu adapter smoke test — E01 spec §5 T2 acceptance criterion.
//!
//! Every CI OS must produce a wgpu adapter: macOS via Metal, Windows via DX12
//! (WARP counts), Linux via Vulkan (Mesa lavapipe counts). A machine with
//! genuinely no adapter can opt out with `LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER=1`
//! — CI never sets it, so a runner losing its software adapter fails loudly.

#[test]
fn a_wgpu_adapter_is_available() {
    // wgpu 30: Backends::default() == all(); no display handle needed headless.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());

    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));

    match adapter {
        Ok(adapter) => {
            let info = adapter.get_info();
            // Surfaced in CI logs so backend/driver regressions are diagnosable.
            println!(
                "wgpu adapter: name={:?} backend={:?} device_type={:?} driver={:?}",
                info.name, info.backend, info.device_type, info.driver
            );
        }
        Err(err) => {
            if std::env::var_os("LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER").is_some() {
                eprintln!(
                    "no wgpu adapter ({err}); tolerated because \
                     LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER is set"
                );
            } else {
                panic!(
                    "no wgpu adapter available: {err}. CI runners must provide one \
                     (Metal / DX12 WARP / Vulkan lavapipe). Set \
                     LIGHTBOX_SMOKE_ALLOW_NO_ADAPTER=1 to skip on genuinely headless machines."
                );
            }
        }
    }
}
