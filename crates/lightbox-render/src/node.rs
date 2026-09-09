// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The `RenderNode` trait surface (spec §3.4), E05.1's stated starting point.
//!
//! Frozen by E01: [`RenderNode`], [`NodeId`], [`Params`], [`NodeRegistry`],
//! and the tile types' *shape*, at M0 one tile == the whole image, but tiles
//! carry `(offset, extent)` so E05.3's 256² tiling changes tile *production*,
//! not this trait.

use std::collections::HashMap;
use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_types::ProcessVersion;

use crate::error::NodeError;
use crate::gpu::GpuContext;
use crate::pool::TexturePool;

/// Stable node identity, e.g. `NodeId("display.transform")` (spec §3.4).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(pub &'static str);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Node parameters: CBOR bytes plus their hash, opaque to the engine, hashed
/// for E05.2's content-keyed cache (spec §3.4).
#[derive(Clone, Debug)]
pub struct Params {
    /// Canonical CBOR encoding of the node's parameter struct.
    pub cbor: Arc<[u8]>,
    /// xxh3-64 of `cbor` (cache key ingredient for E05.2).
    pub hash: u64,
}

impl Params {
    /// Wraps raw CBOR bytes, computing the hash.
    pub fn from_cbor(cbor: impl Into<Arc<[u8]>>) -> Params {
        let cbor = cbor.into();
        let hash = twox_hash::XxHash3_64::oneshot(&cbor);
        Params { cbor, hash }
    }

    /// Encodes a serde value as CBOR params.
    pub fn from_serialize<T: serde::Serialize>(value: &T) -> Result<Params, NodeError> {
        let mut buf = Vec::new();
        ciborium::into_writer(value, &mut buf)
            .map_err(|e| NodeError::BadParams(format!("cbor encode: {e}")))?;
        Ok(Params::from_cbor(buf))
    }

    /// Decodes the CBOR back into the node's parameter struct.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, NodeError> {
        ciborium::from_reader(&*self.cbor)
            .map_err(|e| NodeError::BadParams(format!("cbor decode: {e}")))
    }
}

/// Placeholder for E05.2's cache-invalidation delta: which params changed.
/// M0 nodes conservatively answer `true` to [`RenderNode::invalidates`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ParamDelta {}

/// A GPU tile. M0: one tile == the whole image; `(offset, extent)` exist so
/// E05.3's tiling changes tile production, not this trait (spec §3.4).
#[derive(Clone)]
pub struct Tile {
    /// The texture holding this tile's pixels.
    pub texture: Arc<wgpu::Texture>,
    /// A default view of `texture` (what nodes bind).
    pub view: Arc<wgpu::TextureView>,
    /// Tile origin within the full image, in pixels. M0: `[0, 0]`.
    pub offset: [u32; 2],
    /// Tile size in pixels. M0: the full image size.
    pub extent: [u32; 2],
}

/// A CPU tile (RGBA8, sRGB-encoded), mirror of [`Tile`] for the CPU path.
#[derive(Clone, Debug)]
pub struct TileCpu {
    /// The pixels. `buf` dimensions equal `extent`.
    pub buf: ImageBufU8,
    /// Tile origin within the full image, in pixels. M0: `[0, 0]`.
    pub offset: [u32; 2],
}

/// An owned RGBA8 sRGB-encoded pixel buffer (spec §3.4 `RenderOutput::Cpu`).
#[derive(Clone, PartialEq, Eq)]
pub struct ImageBufU8 {
    /// Tightly packed RGBA8 rows, `4 * width * height` bytes.
    pub px: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl std::fmt::Debug for ImageBufU8 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageBufU8")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.px.len())
            .finish()
    }
}

/// What a node sees during GPU evaluation. Cancel visibility via ctx is one of
/// the two deliberate §2.2 hardening deviations (spec §3.4).
pub struct GpuCtx<'a> {
    /// The shared device/queue the node must create resources on.
    pub gpu: &'a GpuContext,
    /// Where output textures come from (recycled by size, spec T6).
    pub pool: &'a TexturePool,
    /// Cooperative cancellation; long nodes must check at checkpoints.
    pub cancel: &'a CancelToken,
}

/// What a node sees during CPU evaluation.
pub struct CpuCtx<'a> {
    /// Cooperative cancellation; long nodes must check at checkpoints.
    pub cancel: &'a CancelToken,
}

/// A render pipeline node (spec §3.4, frozen surface). §2.2's contract with
/// two hardening deviations: `Result` returns, and cancel visibility via ctx.
pub trait RenderNode: Send + Sync {
    /// The node's stable identity.
    fn id(&self) -> NodeId;
    /// Evaluate on the GPU. Every wgpu resource must be created on
    /// `ctx.gpu.device` (the shared device, architecture §2.3 seam 2).
    fn eval_gpu(&self, ctx: &GpuCtx<'_>, inputs: &[Tile], p: &Params) -> Result<Tile, NodeError>;
    /// Evaluate on the CPU, the same written algorithm spec as `eval_gpu`,
    /// which is what makes the ΔE parity gate meaningful (spec §4.4).
    fn eval_cpu(
        &self,
        ctx: &CpuCtx<'_>,
        inputs: &[TileCpu],
        p: &Params,
    ) -> Result<TileCpu, NodeError>;
    /// Whether a param change invalidates this node's cached output
    /// consumed by E05.2's cache. M0: answer `true` conservatively.
    fn invalidates(&self, changed: &ParamDelta) -> bool;
}

/// Registry keyed `(NodeId, ProcessVersion)` (spec §3.4). Old process
/// versions stay registered forever (architecture §4.5), a removal API
/// deliberately does not exist.
#[derive(Default)]
pub struct NodeRegistry {
    map: HashMap<(NodeId, ProcessVersion), Arc<dyn RenderNode>>,
}

impl NodeRegistry {
    /// An empty registry.
    pub fn new() -> NodeRegistry {
        NodeRegistry::default()
    }

    /// Registers `node` under `(node.id(), pv)`, replacing any previous
    /// registration for that key.
    pub fn register(&mut self, pv: ProcessVersion, node: Arc<dyn RenderNode>) {
        self.map.insert((node.id(), pv), node);
    }

    /// Looks up the node registered under `(id, pv)`.
    pub fn get(&self, id: NodeId, pv: ProcessVersion) -> Option<Arc<dyn RenderNode>> {
        self.map.get(&(id, pv)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_round_trip_and_hash_stability() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct P {
            a: u32,
            b: [f32; 2],
        }
        let p = P {
            a: 7,
            b: [0.5, 1.0],
        };
        let params = Params::from_serialize(&p).unwrap();
        let params2 = Params::from_serialize(&p).unwrap();
        assert_eq!(params.hash, params2.hash, "same value, same hash");
        assert_eq!(params.decode::<P>().unwrap(), p);

        let other = Params::from_serialize(&P {
            a: 8,
            b: [0.5, 1.0],
        })
        .unwrap();
        assert_ne!(params.hash, other.hash, "different value, different hash");
    }

    #[test]
    fn params_decode_rejects_wrong_shape() {
        #[derive(serde::Serialize)]
        struct A {
            x: u32,
        }
        #[derive(serde::Deserialize, Debug)]
        #[allow(dead_code)]
        struct B {
            y: String,
        }
        let params = Params::from_serialize(&A { x: 1 }).unwrap();
        assert!(matches!(params.decode::<B>(), Err(NodeError::BadParams(_))));
    }
}
