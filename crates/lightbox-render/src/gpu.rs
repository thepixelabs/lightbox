// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`GpuContext`] — the one GPU type allowed across the core/shell boundary
//! (architecture §2.3 seam 2; spec §3.4).
//!
//! Two construction paths (spec T5):
//!
//! * **Shell path** — the eframe/egui-wgpu integration creates the device and
//!   hands its parts to [`GpuContext::from_shared_device`]. The engine then
//!   renders on *the shell's* device, which is what makes compositing the
//!   output texture zero-copy: a texture is only bindable by the device that
//!   created it, so egui registering the engine's texture is itself a
//!   same-device proof (wgpu validation rejects cross-device binds).
//! * **Headless path** — [`GpuContext::headless`] owns instance→adapter→device
//!   and degrades to `None` without panicking when no adapter exists
//!   (CI software-adapter fallback; CPU-only engines take `gpu: None`).

use std::sync::Arc;

/// Shared GPU device/queue plus the adapter facts callers key decisions on.
///
/// The engine NEVER creates the device when running under the shell — it
/// receives the shell's device so the output texture composites zero-copy
/// (spec §3.4, verbatim contract).
#[derive(Clone)]
pub struct GpuContext {
    /// The (internally ref-counted) wgpu device. One per process under the
    /// shell; `Arc` so seam crossings are explicit and identity-checkable
    /// via `Arc::ptr_eq` (T7 debug assertion).
    pub device: Arc<wgpu::Device>,
    /// The submission queue paired with `device`.
    pub queue: Arc<wgpu::Queue>,
    /// Which backend the adapter runs on (Metal/DX12/Vulkan/…).
    pub backend: wgpu::Backend,
    /// The limits the device was created with.
    pub limits: wgpu::Limits,
    /// Adapter identity, for logs and diagnostics.
    pub adapter_info: wgpu::AdapterInfo,
}

impl GpuContext {
    /// Shell path: wrap an existing device/queue (eframe's `RenderState`).
    ///
    /// `device` and `queue` must belong to `adapter_info`'s adapter; wgpu
    /// validation will loudly reject cross-device resource use otherwise.
    pub fn from_shared_device(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
    ) -> GpuContext {
        let limits = device.limits();
        let ctx = GpuContext {
            backend: adapter_info.backend,
            device: Arc::new(device),
            queue: Arc::new(queue),
            limits,
            adapter_info,
        };
        tracing::info!(target: "lightbox_render::gpu", "shared-device GpuContext: {}", ctx.adapter_report());
        ctx
    }

    /// Headless path: own instance→adapter→device. Returns `None` (with a
    /// warning log, never a panic) when no adapter exists or the device
    /// request fails — callers then run the engine CPU-only (spec §3.4).
    pub fn headless() -> Option<GpuContext> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = match pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
        ) {
            Ok(adapter) => adapter,
            Err(err) => {
                tracing::warn!(
                    target: "lightbox_render::gpu",
                    "no wgpu adapter available ({err}); engine will run CPU-only"
                );
                return None;
            }
        };
        let descriptor = GpuContext::device_descriptor(&adapter);
        let (device, queue) = match pollster::block_on(adapter.request_device(&descriptor)) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(
                    target: "lightbox_render::gpu",
                    "adapter {:?} refused device request ({err}); engine will run CPU-only",
                    adapter.get_info().name
                );
                return None;
            }
        };
        let adapter_info = adapter.get_info();
        let ctx = GpuContext {
            backend: adapter_info.backend,
            limits: device.limits(),
            device: Arc::new(device),
            queue: Arc::new(queue),
            adapter_info,
        };
        tracing::info!(target: "lightbox_render::gpu", "headless GpuContext: {}", ctx.adapter_report());
        Some(ctx)
    }

    /// The device descriptor BOTH paths request (T5: features/limits set in
    /// one place). The shell passes this to eframe's device-descriptor
    /// callback so the shared device is created with exactly these
    /// features/limits.
    ///
    /// M0 needs no optional features; limits are the WebGPU defaults with the
    /// 2D texture dimension raised toward the adapter's maximum (photos
    /// exceed the 8192 default on modern hardware; software adapters keep
    /// whatever they can actually do).
    pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
        let limits = wgpu::Limits {
            // Whatever the adapter can do — photos exceed the 8192 default on
            // real hardware; software adapters keep their honest maximum.
            max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d,
            ..wgpu::Limits::default()
        };
        wgpu::DeviceDescriptor {
            label: Some("lightbox shared device"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        }
    }

    /// One-line adapter report (backend, name, driver, key limits) — logged by
    /// both construction paths and exposed for the shell overlay / CLI (T5).
    pub fn adapter_report(&self) -> String {
        format!(
            "adapter={:?} backend={:?} type={:?} driver={:?} max_tex2d={} max_buffer={}",
            self.adapter_info.name,
            self.backend,
            self.adapter_info.device_type,
            self.adapter_info.driver,
            self.limits.max_texture_dimension_2d,
            self.limits.max_buffer_size,
        )
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("backend", &self.backend)
            .field("adapter", &self.adapter_info.name)
            .finish_non_exhaustive()
    }
}
