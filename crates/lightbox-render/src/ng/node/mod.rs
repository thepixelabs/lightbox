// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `RenderNode` trait — the node-author contract (spec §3.2).
//!
//! Owner: **A-core** (tasks **A4** trait + descriptor + `AuxRequirements` with
//! default impls + first probe node; **A3** [`param`]; **A5** [`registry`]).
//! Node-authoring epics (E10/E11/E12) implement this trait; the engine book
//! (task F3) documents the kernel/param/cache conventions.

pub mod param;
pub mod registry;

use std::sync::Arc;

use lightbox_jobs::CancelToken;

use crate::ng::error::NodeError;
use crate::ng::gpu::KernelBuilder;
use crate::ng::tile::{CpuTileView, PixelBuf, TileHandle, TileView};
use crate::ng::types::{Extent, NodeId, PortType, Roi, TilePrecision};

pub use param::{
    FieldDecl, ParamBlock, ParamHash, ParamKind, ParamValue, ParamsSchema, ParamsSchemaRef,
};
pub use registry::{NodeRegistry, PvRange};

/// One declared port: a name and its [`PortType`] (spec §3.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PortDecl {
    /// Port name (stable, used in diagnostics and edge wiring).
    pub name: &'static str,
    /// The image type carried on this port.
    pub ty: PortType,
}

/// A node's static description (spec §3.2). Exactly one output port in v1.
pub struct NodeDescriptor {
    /// The node's stable identity.
    pub id: NodeId,
    /// Input ports — name + type; multi-input supported (mask compositing).
    pub inputs: &'static [PortDecl],
    /// The single output port (multi-output: open Q5).
    pub output: PortDecl,
    /// Typed params schema driving [`ParamBlock`] validation + hashing.
    pub params_schema: ParamsSchemaRef,
}

/// ROI back-propagation result: the input region required per input port
/// (spec §3.2 `plan`).
#[derive(Clone, Debug, Default)]
pub struct InputRois(pub Vec<Roi>);

impl InputRois {
    /// The identity mapping: every one of `n` inputs needs exactly `out`.
    pub fn identity(out: Roi, n: usize) -> InputRois {
        InputRois(vec![out; n])
    }
}

/// Cache behavior of a node (spec §3.5). Passthrough (pure-reorder) nodes may
/// return `Never`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CachePolicy {
    /// Cache this node's output (the default).
    Cache,
    /// Never cache (pure reorder / trivial passthrough).
    Never,
}

/// Fast-path invalidation hint (spec §3.2 `affected_by`): which params changed.
/// Content keys make correctness independent of this hint. **A3/B own it.**
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ParamDelta {}

/// Multi-pass scratch / feedback / LUT requirements (spec §3.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AuxRequirements {
    /// Ping-pong intermediates within one eval.
    pub scratch_tiles: u8,
    /// Previous-invocation output (vkdt feedback pattern).
    pub feedback_slot: Option<PortType>,
    /// Small read-only textures/UBOs (curves, HueSat LUTs).
    pub lut_slots: u8,
}

impl AuxRequirements {
    /// No auxiliary resources — the default.
    pub const NONE: AuxRequirements = AuxRequirements {
        scratch_tiles: 0,
        feedback_slot: None,
        lut_slots: 0,
    };
}

/// A salt over a node's WGSL source + algorithm revision; part of the
/// [`crate::ng::CacheKey`] so old cached outputs can never be served for a new
/// kernel (spec §3.2/§3.5).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct KernelSalt(pub blake3::Hash);

/// What a GPU node sees during evaluation (spec §3.2). Record compute
/// dispatches here; write [`GpuEvalCtx::output`].
///
/// **A-gpu completion** (E05-deviations §A-gpu D-gpuctx): the output tile is
/// **pre-acquired by the backend** (from the [`crate::ng::gpu::TilePool`], at the
/// node's output precision/format) and handed in here; the node records its
/// compute dispatch writing into `output`'s `@group(1)` storage texture, and the
/// backend takes the finished handle back out. Pipelines come from the shared
/// [`KernelBuilder`] cache (`@blake3(wgsl)`), so a node's shader is compiled once
/// per process. This context is constructed and consumed entirely within A-gpu
/// territory (the GPU backend builds it; engine-owned node bodies read it), so it
/// carries no cross-wave coupling.
pub struct GpuEvalCtx<'a> {
    /// The shared device the node records dispatches on (§2.3 seam 2).
    pub device: &'a wgpu::Device,
    /// The submission queue paired with `device`.
    pub queue: &'a wgpu::Queue,
    /// The render scale (source→output decimation factor) for this eval.
    pub scale: f32,
    /// Cooperative cancellation; long nodes check at checkpoints.
    pub cancel: &'a CancelToken,
    /// The shared pipeline cache (§3.2 kernel conventions).
    pub kernels: &'a KernelBuilder,
    /// The pre-acquired output tile the node writes into.
    output: TileHandle,
}

impl<'a> GpuEvalCtx<'a> {
    /// Construct an eval context around a pre-acquired `output` tile (the
    /// backend owns tile acquisition; the node only writes).
    pub fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        kernels: &'a KernelBuilder,
        scale: f32,
        cancel: &'a CancelToken,
        output: TileHandle,
    ) -> GpuEvalCtx<'a> {
        GpuEvalCtx {
            device,
            queue,
            scale,
            cancel,
            kernels,
            output,
        }
    }

    /// The output tile the node writes (`@group(1)` storage texture). The tile
    /// was acquired by the backend at the node's output precision/format.
    pub fn output(&mut self) -> &mut TileHandle {
        &mut self.output
    }

    /// Take the (written) output tile back out — the backend calls this after
    /// [`RenderNode::eval_gpu`] returns to hand the result to the cache/executor.
    pub fn into_output(self) -> TileHandle {
        self.output
    }
}

/// What a CPU node sees during evaluation (spec §3.2). The rayon parity path.
///
/// The [`crate::ng::exec::cpu::CpuBackend`] pre-allocates the output working
/// tile (sized from the eval ROI, formatted per [`RenderNode::precision`]) and
/// hands the node a mutable view through [`CpuEvalCtx::output`]; the node fills
/// it (task A14).
pub struct CpuEvalCtx<'a> {
    /// The render scale (source→output decimation factor) for this eval.
    pub scale: f32,
    /// Cooperative cancellation; long nodes check at checkpoints.
    pub cancel: &'a CancelToken,
    /// The output tile ROI (source-pixel coordinates).
    pub out_roi: Roi,
    /// The pre-allocated output working tile the node writes.
    out: &'a mut PixelBuf,
}

impl<'a> CpuEvalCtx<'a> {
    /// Build a CPU eval context over a pre-allocated output buffer (A-core /
    /// [`crate::ng::exec::cpu::CpuBackend`]).
    pub(crate) fn new(
        scale: f32,
        cancel: &'a CancelToken,
        out_roi: Roi,
        out: &'a mut PixelBuf,
    ) -> CpuEvalCtx<'a> {
        CpuEvalCtx {
            scale,
            cancel,
            out_roi,
            out,
        }
    }

    /// The output buffer the node writes (working-format tile).
    pub fn output(&mut self) -> &mut PixelBuf {
        self.out
    }

    /// The output tile extent (convenience for eval loops).
    pub fn out_extent(&self) -> Extent {
        self.out.extent
    }
}

/// A render pipeline node (spec §3.2). Refines the §2.2 sketch: fallible eval,
/// explicit contexts, ROI back-propagation (`plan`), and coordinate-frame
/// support (`output_extent`).
pub trait RenderNode: Send + Sync {
    /// The node's static description.
    fn descriptor(&self) -> &NodeDescriptor;

    /// ROI back-propagation: input regions required to produce `out` at
    /// `scale`. Default: identity. A radius-`r` neighborhood node returns
    /// `out.expand(ceil(r * scale))`.
    fn plan(&self, out: Roi, scale: f32, params: &ParamBlock) -> InputRois {
        let _ = (scale, params);
        InputRois::identity(out, self.descriptor().inputs.len())
    }

    /// Output extent given input extents — identity by default; crop/rotate
    /// (E11.4) override.
    fn output_extent(&self, inputs: &[Extent], params: &ParamBlock) -> Extent {
        let _ = params;
        inputs.first().copied().unwrap_or(Extent { w: 0, h: 0 })
    }

    /// GPU evaluation: record compute dispatches into `ctx`; write
    /// `ctx.output()`. MUST be apron-correct (tiled eval == whole-image eval,
    /// same backend).
    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError>;

    /// CPU evaluation: same algorithm spec as the WGSL kernel (§4.4).
    /// rayon-safe, allocation-light. Parity: within ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB
    /// of `eval_gpu`.
    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError>;

    /// Cache behavior. Default `Cache`.
    fn cache_policy(&self) -> CachePolicy {
        CachePolicy::Cache
    }

    /// Fast-path invalidation hint: does this delta change output? Default
    /// `true` (conservative). Content keys keep correctness independent of it.
    fn affected_by(&self, delta: &ParamDelta) -> bool {
        let _ = delta;
        true
    }

    /// Output tile precision. Default `F16`; `F32` for accumulation-sensitive
    /// stages.
    fn precision(&self) -> TilePrecision {
        TilePrecision::F16
    }

    /// Multi-pass nodes (guided filter, E10.2) request scratch/persistent slots.
    fn aux_requirements(&self, params: &ParamBlock) -> AuxRequirements {
        let _ = params;
        AuxRequirements::NONE
    }
}

/// Constructs node instances and reports the kernel salt that keys their cache
/// (spec §3.2).
pub trait NodeFactory: Send + Sync {
    /// A fresh node instance.
    fn instantiate(&self) -> Arc<dyn RenderNode>;
    /// Hash of WGSL source + algorithm rev; part of the [`crate::ng::CacheKey`].
    fn kernel_salt(&self) -> KernelSalt;
}
