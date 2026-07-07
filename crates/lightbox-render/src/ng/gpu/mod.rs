// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! GPU device context, kernel builder, pipeline cache, tile pool (spec §2;
//! tasks **A6**/**A7**).
//!
//! Owner: **A-gpu**. Kernel conventions (spec §3.2): one WGSL entry per pass,
//! 16×16×1 workgroups, `@group(0)` inputs / `@group(1)` write-only output /
//! `@group(2)` params UBO / `@group(3)` LUT-aux; shaders embedded via
//! `include_str!`, naga-validated at build (task A6, see `build.rs`). No `f16`
//! arithmetic in WGSL v1 (storage `rgba16float`, compute f32 — Risk R2).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ng::cache::Bytes;
use crate::ng::error::NodeError;
use crate::ng::tile::TileHandle;
use crate::ng::types::{Extent, TilePrecision};

/// A wrapper over the shared wgpu device/queue plus engine-owned GPU state
/// (limits). The engine renders on the **shell's** device so the output texture
/// composites zero-copy (§2.3 seam 2).
pub struct DeviceCtx {
    /// The shared device (from the [`crate::ng::DeviceProvider`] seam).
    pub device: Arc<wgpu::Device>,
    /// The submission queue paired with `device`.
    pub queue: Arc<wgpu::Queue>,
    /// The limits `device` was created with (A6 — kernels size dispatches and
    /// tiles against these).
    pub limits: wgpu::Limits,
}

impl DeviceCtx {
    /// Wraps a shared device/queue pair.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> DeviceCtx {
        let limits = device.limits();
        DeviceCtx {
            device,
            queue,
            limits,
        }
    }
}

/// The workgroup edge the §3.2 conventions mandate (16×16×1).
pub const WORKGROUP: u32 = 16;

/// Builds compute pipelines under the §3.2 bind-group conventions, caching by
/// `blake3(wgsl ‖ entry)` (spec task **A6**).
///
/// Pipelines use an **auto-derived** bind-group layout (`layout: None`): naga
/// infers `@group(0)` sampled inputs, `@group(1)` write-only storage output,
/// `@group(2)` uniform params, and `@group(3)` LUT/aux storage from the shader
/// itself, so node authors bind exactly the groups their kernel declares. Every
/// shader shipped under `shaders/` is already naga-validated at build time
/// (`build.rs`); runtime creation is wrapped in a validation error scope so a
/// malformed *dynamic* kernel surfaces as [`NodeError::Gpu`] rather than a panic.
pub struct KernelBuilder {
    device: Arc<wgpu::Device>,
    cache: Mutex<HashMap<[u8; 32], wgpu::ComputePipeline>>,
}

impl KernelBuilder {
    /// A kernel builder for `device`.
    pub fn new(device: &DeviceCtx) -> KernelBuilder {
        KernelBuilder {
            device: Arc::clone(&device.device),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// A compute pipeline for `wgsl`'s `entry` point, cached by
    /// `blake3(wgsl ‖ entry)`. Invalid WGSL fails the build (naga); a dynamic
    /// kernel that fails validation at runtime returns [`NodeError::Gpu`].
    pub fn compute_pipeline(
        &self,
        wgsl: &str,
        entry: &str,
    ) -> Result<wgpu::ComputePipeline, NodeError> {
        let key = {
            let mut h = blake3::Hasher::new();
            h.update(wgsl.as_bytes());
            h.update(b"\0");
            h.update(entry.as_bytes());
            *h.finalize().as_bytes()
        };

        if let Some(pipe) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return Ok(pipe.clone());
        }

        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(entry),
                source: wgpu::ShaderSource::Wgsl(wgsl.into()),
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(NodeError::Gpu(format!(
                "compute pipeline '{entry}' failed validation: {err}"
            )));
        }

        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, pipeline.clone());
        Ok(pipeline)
    }

    /// Number of distinct pipelines resident in the cache (diagnostics/tests).
    pub fn cached_len(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// Records one 16×16×1-workgroup compute pass over an `out`-sized grid: binds
/// `groups[i]` at `@group(i)` (contiguous from 0), dispatches `ceil(out/16)`
/// workgroups, and submits on `queue`. The shared §3.2 dispatch primitive every
/// engine-owned node uses.
pub fn dispatch_compute(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::ComputePipeline,
    groups: &[&wgpu::BindGroup],
    out: Extent,
    label: &str,
) {
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        for (i, bg) in groups.iter().enumerate() {
            pass.set_bind_group(i as u32, *bg, &[]);
        }
        pass.dispatch_workgroups(
            out.w.max(1).div_ceil(WORKGROUP),
            out.h.max(1).div_ceil(WORKGROUP),
            1,
        );
    }
    queue.submit([encoder.finish()]);
}

/// The wgpu texture format a working-tile [`TilePrecision`] maps to (§4.2):
/// `F16` → `rgba16float`, `F32` → `rgba32float`.
pub fn working_format(precision: TilePrecision) -> wgpu::TextureFormat {
    match precision {
        TilePrecision::F16 => wgpu::TextureFormat::Rgba16Float,
        TilePrecision::F32 => wgpu::TextureFormat::Rgba32Float,
    }
}

/// Usage every pooled working/output tile is created with: written by compute
/// (`STORAGE_BINDING`), read by downstream nodes (`TEXTURE_BINDING`), uploaded
/// into (`COPY_DST`), and read back / composited (`COPY_SRC`).
const TILE_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::STORAGE_BINDING
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::COPY_SRC);

fn format_bytes_per_pixel(format: wgpu::TextureFormat) -> u64 {
    match format {
        wgpu::TextureFormat::Rgba32Float => 16,
        wgpu::TextureFormat::Rgba16Float => 8,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => 4,
        wgpu::TextureFormat::R16Float => 2,
        // Working/display/weight formats are the only ones the pool issues; any
        // other is conservatively billed at the working-tile cost.
        _ => 8,
    }
}

struct PooledTex {
    texture: Arc<wgpu::Texture>,
    extent: Extent,
    format: wgpu::TextureFormat,
    precision: TilePrecision,
    bytes: u64,
    last_used: u64,
}

impl PooledTex {
    /// A pooled texture is idle (reclaimable) when only the pool holds it — every
    /// live [`TileHandle`] carries its own strong reference to this `Arc`.
    fn is_idle(&self) -> bool {
        Arc::strong_count(&self.texture) == 1
    }
}

struct PoolInner {
    textures: Vec<PooledTex>,
    used_bytes: u64,
    peak_bytes: u64,
    clock: u64,
}

/// A pool of `rgba16float` / `rgba32float` (and display `rgba8unorm`) working
/// tiles with byte-budget accounting and LRU reclaim of the idle free-list
/// (spec task **A7**).
///
/// Reuse is strong-count based (identical to the E01 seed pool): a pooled
/// texture whose `Arc` strong count is 1 is held only by the pool and may be
/// re-issued for a matching (extent, format); acquiring a new texture that would
/// breach the budget first LRU-evicts idle textures. Peak residency is tracked
/// for the §C7 VRAM gate.
pub struct TilePool {
    device: Arc<wgpu::Device>,
    budget: Bytes,
    inner: Mutex<PoolInner>,
}

impl TilePool {
    /// A pool creating tiles on `device`, capped at `budget`.
    pub fn new(device: Arc<wgpu::Device>, budget: Bytes) -> TilePool {
        TilePool {
            device,
            budget,
            inner: Mutex::new(PoolInner {
                textures: Vec::new(),
                used_bytes: 0,
                peak_bytes: 0,
                clock: 0,
            }),
        }
    }

    /// Acquires a working tile of `extent` at `precision` (reused from the
    /// free-list when possible). Holding the returned handle keeps it out of
    /// reuse; dropping every clone returns it to the free-list.
    pub fn acquire(&self, extent: Extent, precision: TilePrecision) -> TileHandle {
        self.acquire_format(extent, working_format(precision), precision)
    }

    /// Acquires a tile of `extent` in an explicit `format` (e.g. the display
    /// node's `rgba8unorm` output). `precision` is carried on the handle for
    /// cache-key purposes; it need not match `format`'s bit depth.
    pub fn acquire_format(
        &self,
        extent: Extent,
        format: wgpu::TextureFormat,
        precision: TilePrecision,
    ) -> TileHandle {
        let w = extent.w.max(1);
        let h = extent.h.max(1);
        let extent = Extent { w, h };
        let bytes = u64::from(w) * u64::from(h) * format_bytes_per_pixel(format);

        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.clock += 1;
        let now = inner.clock;

        // Reuse an idle texture of the exact shape.
        if let Some(slot) = inner
            .textures
            .iter_mut()
            .find(|t| t.is_idle() && t.extent == extent && t.format == format)
        {
            slot.last_used = now;
            slot.precision = precision;
            let texture = Arc::clone(&slot.texture);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            return TileHandle::gpu(texture, view, extent, precision, format);
        }

        // No reuse: LRU-evict idle textures until the newcomer fits the budget.
        self.evict_to_fit(&mut inner, bytes);

        let texture = Arc::new(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lightbox working tile"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: TILE_USAGE,
            view_formats: &[],
        }));
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        inner.textures.push(PooledTex {
            texture: Arc::clone(&texture),
            extent,
            format,
            precision,
            bytes,
            last_used: now,
        });
        inner.used_bytes += bytes;
        inner.peak_bytes = inner.peak_bytes.max(inner.used_bytes);
        TileHandle::gpu(texture, view, extent, precision, format)
    }

    /// Evicts least-recently-used idle textures until `incoming` bytes fit under
    /// the budget (or no idle textures remain).
    fn evict_to_fit(&self, inner: &mut PoolInner, incoming: u64) {
        let budget = self.budget.0;
        while inner.used_bytes + incoming > budget {
            let victim = inner
                .textures
                .iter()
                .enumerate()
                .filter(|(_, t)| t.is_idle())
                .min_by_key(|(_, t)| t.last_used)
                .map(|(i, _)| i);
            match victim {
                Some(i) => {
                    let removed = inner.textures.swap_remove(i);
                    inner.used_bytes -= removed.bytes;
                }
                None => break, // nothing idle to reclaim
            }
        }
    }

    /// The configured byte budget.
    pub fn budget(&self) -> Bytes {
        self.budget
    }

    /// Bytes currently resident (all pooled textures, idle or in use).
    pub fn used(&self) -> Bytes {
        Bytes(
            self.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .used_bytes,
        )
    }

    /// Peak resident bytes since construction (the §C7 VRAM-gate tracker).
    pub fn peak(&self) -> Bytes {
        Bytes(
            self.inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .peak_bytes,
        )
    }

    /// Number of textures the pool currently owns (diagnostics/tests).
    pub fn texture_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .textures
            .len()
    }
}
