// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.wb` (E10 task **A9**). White balance: raw path is spec'd as
//! per-channel gains from resolved temp/tint via E02 camera matrices;
//! non-raw path is a Bradford CAT applied in working RGB (spec §4.2).
//!
//! # The only reachable path today
//!
//! `SourceImage` pixels arrive at the render graph **already demosaiced and
//! already in working space**, the M1 reality documented on
//! [`crate::ng::colorimetry::SourceKind`] ("the M1 reality, E02 returns
//! demosaiced RGB") and on [`crate::ng::compile::RecipeCompiler::compile`]
//! ("`SourceDesc` does not yet drive stage selection (raw vs non-raw WB path,
//! E10 task A9)"). No raw sensor mosaic reaches this graph yet
//! (`SourceKind::Raw` is E11-reserved), so [`WhiteBalanceNode`] always
//! executes the **non-raw** branch (spec §4.2's own domain note: "display-
//! referred input already in working space", literally true of every pixel
//! that reaches this node in M1). The raw-path per-channel-gain math
//! ([`lightbox_color::wb::gains_from_temp_tint`]) is implemented and
//! property-tested (task A10) so E11's future raw-mosaic seam can wire it in
//! with no redesign of the param model. Recorded in
//! `docs/plan/epics/E10-deviations.md`.
//!
//! # As-shot = identity
//!
//! `WhiteBalance::AsShot` (the default) and `WhiteBalance::Auto` (no solve
//! exists yet, Phase E's `AutoWhiteBalance` command) both resolve to
//! [`GlobalNode::is_identity`] `true`: the node is never added to the graph,
//! so an as-shot render is **structurally** identical to the pre-A9 M1
//! baseline (`build_wb_segment`'s old always-elide stub), byte-identical by
//! construction, not by coincidence.
//!
//! `WhiteBalance::Preset`/`Custom` resolve to a `(kelvin, tint)` pair (preset
//! table lookup or the literal custom value) and apply
//! [`lightbox_color::wb::non_raw_wb_matrix`] as a 3×3 matrix multiply on
//! working RGB; WGSL (`shaders/global_white_balance.wgsl`) and CPU
//! ([`apply_wb_matrix`]) evaluate the identical row·vector formula from the
//! identical 9 baked matrix params, so CPU/GPU parity (§4.4) holds to float
//! rounding.

use std::sync::Arc;

use lightbox_color::wb::non_raw_wb_matrix;
use lightbox_edit::{GlobalStages, WbPreset as EditWbPreset, WhiteBalance};

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The white-balance matrix kernel, naga-validated at build (`build.rs`).
const WHITE_BALANCE_WGSL: &str = include_str!("../../../../shaders/global_white_balance.wgsl");

/// The 9 baked matrix-entry param fields, row-major (`m<row><col>`).
static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "m00",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m01",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m02",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m10",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m11",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m12",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m20",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m21",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m22",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.wb"),
    inputs: &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }],
    output: PortDecl {
        name: "out",
        ty: PortType::LinearRgbaF16,
    },
    params_schema: ParamsSchemaRef(&SCHEMA),
};

/// Unreachable fallback `(kelvin, tint)`, [`WhiteBalanceNode::param_block`]
/// is only ever invoked when [`resolve_temp_tint`] returned `Some` (the
/// `is_identity` gate in `nodes/global/mod.rs::maybe_add` filters the `None`
/// case out before `param_block` is called), so this is never actually
/// read; it is the working space's own approximate white so the matrix would
/// be near-identity even if this path were ever hit.
const FALLBACK_KELVIN: f64 = 5003.0;

/// Maps the `lightbox-edit` preset vocabulary onto `lightbox-color`'s (spec
/// §3.3 CCT anchors), a small crate-boundary adapter, both enums share the
/// same 6 names by construction (E09/E02 §3.2 alignment).
fn color_preset(p: EditWbPreset) -> lightbox_color::wb::WbPreset {
    use lightbox_color::wb::WbPreset as C;
    match p {
        EditWbPreset::Daylight => C::Daylight,
        EditWbPreset::Cloudy => C::Cloudy,
        EditWbPreset::Shade => C::Shade,
        EditWbPreset::Tungsten => C::Tungsten,
        EditWbPreset::Fluorescent => C::Fluorescent,
        EditWbPreset::Flash => C::Flash,
        // `WbPreset` is `#[non_exhaustive]`, a future preset this build
        // doesn't know about degrades to Daylight rather than failing to
        // compile; forward-compat, matches `panels::basic::wb_mode_label`'s
        // own non_exhaustive handling convention.
        _ => C::Daylight,
    }
}

/// Resolves `wb` to the `(kelvin, tint)` pair [`WhiteBalanceNode`] should
/// apply, or `None` when it's a structural no-op (spec A9 "as-shot renders
/// byte-identical"). The single source of truth both
/// [`GlobalNode::is_identity`] and [`GlobalNode::param_block`] read, so the
/// two can never disagree about which `WhiteBalance` values are identity.
///
/// `pub(crate)` (not private): task **D1**'s `AutoBwMix`
/// (`nodes::global::bw_mix::auto_bw_mix`) reuses this exact resolver so its
/// "WB-informed" deterministic mix reads the same resolved Kelvin this node
/// itself would apply, one resolution rule, not two that could silently
/// drift apart.
pub(crate) fn resolve_temp_tint(wb: &WhiteBalance) -> Option<(f64, f64)> {
    match wb {
        // As-shot is already resolved upstream (E02's decode baked the
        // as-shot WB into the working-space pixels this node receives)
        // and `Auto` has no solve yet (Phase E task E3), both no-ops here.
        WhiteBalance::AsShot | WhiteBalance::Auto => None,
        WhiteBalance::Preset(preset) => {
            let target = color_preset(*preset);
            lightbox_color::wb::wb_presets()
                .iter()
                .find(|(p, _, _)| *p == target)
                .map(|(_, kelvin, tint)| (*kelvin, *tint))
        }
        WhiteBalance::Custom { temp_k, tint } => Some((*temp_k as f64, *tint as f64)),
        // `WhiteBalance` is `#[non_exhaustive]`, a future variant this
        // build doesn't know about degrades to identity rather than
        // guessing at its semantics.
        _ => None,
    }
}

/// Applies a row-major 3×3 matrix to working-space RGB (alpha untouched)
/// the CPU parity anchor for `global_white_balance.wgsl`'s `dot(row, rgb)`
/// formula.
#[inline]
pub fn apply_wb_matrix(rgba: [f32; 4], m: &[[f32; 3]; 3]) -> [f32; 4] {
    let r = m[0][0] * rgba[0] + m[0][1] * rgba[1] + m[0][2] * rgba[2];
    let g = m[1][0] * rgba[0] + m[1][1] * rgba[1] + m[1][2] * rgba[2];
    let b = m[2][0] * rgba[0] + m[2][1] * rgba[1] + m[2][2] * rgba[2];
    [r, g, b, rgba[3]]
}

/// Reads the 9 baked `m<row><col>` fields back into a matrix, the shared
/// decode both `eval_gpu`'s UBO write and `eval_cpu` use, so the two
/// backends can never drift on which bytes mean what.
fn matrix_from_params(params: &ParamBlock) -> [[f32; 3]; 3] {
    let mut m = [[0f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            let key = format!("m{i}{j}");
            let default = if i == j { 1.0 } else { 0.0 };
            *cell = params.get_f64_or(&key, default) as f32;
        }
    }
    m
}

/// The `global.wb` develop node.
#[derive(Default)]
pub struct WhiteBalanceNode {}

impl WhiteBalanceNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.wb");

    /// A fresh node.
    pub fn new() -> WhiteBalanceNode {
        WhiteBalanceNode::default()
    }
}

impl RenderNode for WhiteBalanceNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    fn eval_gpu(
        &self,
        ctx: &mut GpuEvalCtx<'_>,
        inputs: &[TileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Other("global.wb: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.wb: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let m = matrix_from_params(params);
        let mut ubo_bytes = [0u8; 48];
        for (i, row) in m.iter().enumerate() {
            let packed = [row[0], row[1], row[2], 0.0f32];
            for (j, v) in packed.iter().enumerate() {
                let off = i * 16 + j * 4;
                ubo_bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.wb params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        let pipeline = ctx.kernels.compute_pipeline(WHITE_BALANCE_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.wb in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.wb out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.wb params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params],
            extent,
            "global.wb",
        );
        Ok(())
    }

    fn eval_cpu(
        &self,
        ctx: &mut CpuEvalCtx<'_>,
        inputs: &[CpuTileView<'_>],
        params: &ParamBlock,
    ) -> Result<(), NodeError> {
        if ctx.cancel.is_cancelled() {
            return Err(NodeError::Cancelled);
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.wb: missing input tile".into()))?
            .pixels;
        let m = matrix_from_params(params);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_wb_matrix(p, &m);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for WhiteBalanceNode {
    fn is_identity(p: &GlobalStages) -> bool {
        resolve_temp_tint(&p.white_balance).is_none()
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let (kelvin, tint) = resolve_temp_tint(&p.white_balance).unwrap_or((FALLBACK_KELVIN, 0.0));
        let m = non_raw_wb_matrix(kelvin, tint).0;
        let mut fields: Vec<(String, ParamValue)> = Vec::with_capacity(9);
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                fields.push((format!("m{i}{j}"), ParamValue::Float(*v)));
            }
        }
        ParamBlock::from_fields(fields)
            .expect("wb matrix entries are always finite (Bradford CAT of finite chromaticities)")
    }
}

/// Factory registering [`WhiteBalanceNode`].
#[derive(Default)]
pub struct WhiteBalanceFactory {}

impl NodeFactory for WhiteBalanceFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(WhiteBalanceNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(WHITE_BALANCE_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_shot_and_auto_are_identity() {
        let mut g = GlobalStages::default();
        assert!(WhiteBalanceNode::is_identity(&g));
        g.white_balance = WhiteBalance::Auto;
        assert!(WhiteBalanceNode::is_identity(&g));
    }

    #[test]
    fn preset_and_custom_are_not_identity() {
        let g = GlobalStages {
            white_balance: WhiteBalance::Preset(EditWbPreset::Tungsten),
            ..GlobalStages::default()
        };
        assert!(!WhiteBalanceNode::is_identity(&g));
        let g = GlobalStages {
            white_balance: WhiteBalance::Custom {
                temp_k: 8000.0,
                tint: 10.0,
            },
            ..GlobalStages::default()
        };
        assert!(!WhiteBalanceNode::is_identity(&g));
    }

    #[test]
    fn param_block_carries_the_resolved_matrix() {
        let g = GlobalStages {
            white_balance: WhiteBalance::Custom {
                temp_k: 8000.0,
                tint: 0.0,
            },
            ..GlobalStages::default()
        };
        let pb = WhiteBalanceNode::param_block(&g);
        let expected = non_raw_wb_matrix(8000.0, 0.0).0;
        for (i, row) in expected.iter().enumerate() {
            for (j, want) in row.iter().enumerate() {
                let got = pb.get_f64(&format!("m{i}{j}")).unwrap();
                assert!((got - want).abs() < 1e-9);
            }
        }
    }

    /// CPU parity anchor: `apply_wb_matrix` applied through
    /// `matrix_from_params(WhiteBalanceNode::param_block(g))` matches a
    /// direct `non_raw_wb_matrix` application on the same pixel, the round
    /// trip through the `ParamBlock` encoding loses nothing beyond f32
    /// rounding.
    #[test]
    fn param_round_trip_matches_direct_matrix_application() {
        let g = GlobalStages {
            white_balance: WhiteBalance::Preset(EditWbPreset::Tungsten),
            ..GlobalStages::default()
        };
        let pb = WhiteBalanceNode::param_block(&g);
        let m = matrix_from_params(&pb);
        let px = [0.4f32, 0.4, 0.4, 1.0];
        let via_node = apply_wb_matrix(px, &m);

        let (kelvin, tint) = resolve_temp_tint(&g.white_balance).unwrap();
        let direct = non_raw_wb_matrix(kelvin, tint);
        let mv = direct.mul_vec(lightbox_color::Vec3([
            px[0] as f64,
            px[1] as f64,
            px[2] as f64,
        ]));
        for (a, b) in via_node
            .iter()
            .zip([mv.0[0] as f32, mv.0[1] as f32, mv.0[2] as f32, 1.0])
        {
            assert!((a - b).abs() < 1e-4, "via_node={via_node:?}");
        }
    }
}
