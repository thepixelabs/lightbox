// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.creative_lut` (E10 task **D9**). 3D-LUT tetrahedral sampling of a
//! creative-look pack (spec §4.8), applied in the companion-encoded domain
//! (§4.2: "these packs are authored display-referred", the exact
//! `companion_encode`/`companion_decode` bracket `nodes::global::tone_curve`
//! uses), with an `amount` control that interpolates identity↔LUT for
//! `amount ∈ [0,1]` and linearly extrapolates (clamped in-gamut) for `amount
//! ∈ (1,2]` (spec §4.8's own "0-200%" contract).
//!
//! # The `id` → LUT-content seam (D10, explicitly out of THIS phase's scope)
//!
//! [`lightbox_edit::CreativeLut`] (the recipe field this node reads) carries
//! only `{ id: String, amount: f32 (0..=100, unity 100) }`, a *reference*,
//! not the LUT's actual table. Resolving `id` to real `.cube`/HaldCLUT bytes
//! is the `installed_look` catalog DAO's job (spec §5.2, task **D10**), which
//! does not exist yet and is a **hard exclusion** of this phase (no DB/
//! migration/DAO work here). [`CreativeLutNode::param_block`], the only
//! entry point [`crate::ng::nodes::global::build_tone_color_segment`] can
//! call from a real [`GlobalStages`], therefore has no way to obtain real
//! table content today and carries `amount` + the raw `id` (for future
//! diagnostics) only.
//!
//! This is not a silent gap: [`CreativeLutNode::eval_cpu`]/`eval_gpu`
//! default to [`Lut3D::identity`] whenever no table was encoded into the
//! [`ParamBlock`] they were handed, which is **exactly** spec §4.8's own
//! documented missing-look contract, "a missing look renders as identity +
//! a non-modal badge, and the parameter survives", just reached today via
//! "no resolver wired yet" instead of "hash not found in the catalog". Once
//! D10 exists, its wiring only has to call
//! [`CreativeLutNode::param_block_with_lut`] with the resolved [`Lut3D`]
//! the exact seam this phase's own tests exercise directly (see
//! `tests/e10_creative_lut.rs`), proving the node is fully functional ahead
//! of D10 landing. Recorded in `docs/plan/epics/E10-deviations.md` per the
//! brief's instruction.
//!
//! # Testability ahead of D10 (the brief's explicit allowance)
//!
//! [`CreativeLutNode::param_block_with_lut`] is the direct "feed a synthetic
//! LUT through the param/test harness" seam: it encodes a concrete
//! [`Lut3D`] (identity or a known non-identity table) plus `amount` into a
//! [`ParamBlock`] the SAME `eval_cpu`/`eval_gpu` consume, bypassing the
//! (not-yet-existing) `id`-resolution path entirely, so the node's tetrahedral
//! math, amount interpolation/extrapolation, and CPU/GPU parity are all
//! provable today.

use std::fmt::Write as _;
use std::sync::Arc;

use lightbox_color::matrix::spaces::{companion_decode, companion_encode};
use lightbox_edit::GlobalStages;

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::common::lut3d::Lut3D;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The tetrahedral-sample kernel, naga-validated at build (`build.rs`).
const CREATIVE_LUT_WGSL: &str = include_str!("../../../../shaders/global_creative_lut.wgsl");

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "amount",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "look_id",
        kind: ParamKind::Text,
    },
    FieldDecl {
        name: "lut_size",
        kind: ParamKind::Int,
    },
    FieldDecl {
        name: "lut_data",
        kind: ParamKind::Text,
    },
    FieldDecl {
        name: "domain_min_r",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "domain_min_g",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "domain_min_b",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "domain_max_r",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "domain_max_g",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "domain_max_b",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.creative_lut"),
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

/// A 2-entry identity cube, the degrade-to-passthrough default whenever a
/// [`ParamBlock`] carries no (or an unparseable) LUT table (see the module
/// doc's "D10 seam" section). `sample_tetrahedral` on this table is the exact
/// identity function on `[0,1]^3`.
fn fallback_identity_lut() -> Lut3D {
    Lut3D::identity(2)
}

/// Resolves an installed look's content-hash id (a recipe's
/// `CreativeLut.id`) to its parsed 3D LUT table at graph-compile time (task
/// **D10**, the module doc's "the `id` → LUT-content seam"). Implemented by
/// `lightbox-core`'s catalog-backed `CatalogLookResolver` over the
/// `installed_look` table; injected into [`crate::ng::RecipeCompiler`] via
/// `RecipeCompiler::with_look_resolver` and threaded down to
/// [`crate::ng::nodes::global::build_tone_color_segment`], which is the ONLY
/// caller (`nodes::global::mod`'s `maybe_add_creative_lut`).
///
/// `None` (no resolver configured, every caller other than
/// `lightbox-core`'s production `Engine`, e.g. tests/`lightbox-cli`) or
/// [`LookResolver::resolve`] returning `None` (hash not installed, or its
/// file failed to re-parse) both leave [`CreativeLutNode`] on its documented
/// graceful-identity path (`resolve_from_params`'s existing fallback)
/// exactly spec §4.8's missing-look contract, reached via "no resolver" or
/// "hash not found" identically. This trait does **not** touch the node's
/// math/kernel: it only supplies the `Lut3D` [`CreativeLutNode::
/// param_block_with_lut`] (already merged, D9) encodes into the
/// [`ParamBlock`] both backends read.
pub trait LookResolver: Send + Sync {
    /// Resolves `content_hash` (the recipe's `CreativeLut.id`) to a parsed
    /// LUT, or `None` if it isn't installed / fails to re-parse.
    fn resolve(&self, content_hash: &str) -> Option<Arc<Lut3D>>;
}

/// Recipe-domain amount (`0..=100`%, unity `100`, spec doc on
/// `lightbox_edit::leaves::CreativeLut::amount`) → the node's own
/// `0.0..=2.0` fraction domain (spec §4.8), the exact conversion
/// [`CreativeLutNode::param_block`] applies internally, extracted as a pure
/// function so the D10 resolver-wiring call site
/// (`nodes::global::mod::maybe_add_creative_lut`) can build a [`ParamBlock`]
/// via [`CreativeLutNode::param_block_with_lut`] without re-deriving it or
/// reaching into `param_block`'s own (untouched) body.
pub fn recipe_amount_to_fraction(recipe_amount: f32) -> f32 {
    (recipe_amount as f64 / 100.0).clamp(0.0, 2.0) as f32
}

/// Encodes [`Lut3D::data`] as `"r,g,b;r,g,b;..."`, the same compact `Text`
/// carrier convention `nodes::global::tone_curve::encode_points` uses (no
/// variable-length array [`ParamValue`] variant exists; [`ParamBlock`] is
/// deliberately scalar-only, spec §3.1).
fn encode_lut_data(lut: &Lut3D) -> String {
    let mut s = String::new();
    for (i, c) in lut.data().iter().enumerate() {
        if i > 0 {
            s.push(';');
        }
        // `write!` to a String never fails.
        let _ = write!(s, "{},{},{}", c[0], c[1], c[2]);
    }
    s
}

/// Inverse of [`encode_lut_data`]. Never panics on malformed input: any
/// unparseable/non-finite token drops that entry rather than failing the
/// whole decode (mirrors `tone_curve::decode_points`'s defensive contract)
/// the caller checks the resulting length against `size^3` and degrades to
/// [`fallback_identity_lut`] on any mismatch.
fn decode_lut_data(s: &str) -> Vec<[f32; 3]> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(';')
        .filter_map(|tok| {
            let mut it = tok.split(',');
            let r: f32 = it.next()?.parse().ok()?;
            let g: f32 = it.next()?.parse().ok()?;
            let b: f32 = it.next()?.parse().ok()?;
            if it.next().is_some() {
                return None;
            }
            if r.is_finite() && g.is_finite() && b.is_finite() {
                Some([r, g, b])
            } else {
                None
            }
        })
        .collect()
}

/// Resolves `(Lut3D, amount)` from a [`ParamBlock`], the ONE place both
/// `eval_cpu` and `eval_gpu` derive their LUT + amount from, so the two
/// backends can never see different data (mirrors `tone_curve::bake_from_params`'s
/// "single source" convention). Any missing/malformed/size-mismatched LUT
/// data degrades to [`fallback_identity_lut`] rather than panicking or
/// indexing out of bounds, the D10-seam graceful-degrade contract (module
/// doc).
fn resolve_from_params(params: &ParamBlock) -> (Lut3D, f32) {
    let amount = params.get_f64_or("amount", 0.0) as f32;
    let size = params.get_i64("lut_size").unwrap_or(0);
    if size < i64::from(super::super::common::lut3d::MIN_LUT_SIZE) {
        return (fallback_identity_lut(), amount);
    }
    let size = size as u32;
    let data = decode_lut_data(params.get_str("lut_data").unwrap_or(""));
    let domain_min = [
        params.get_f64_or("domain_min_r", 0.0) as f32,
        params.get_f64_or("domain_min_g", 0.0) as f32,
        params.get_f64_or("domain_min_b", 0.0) as f32,
    ];
    let domain_max = [
        params.get_f64_or("domain_max_r", 1.0) as f32,
        params.get_f64_or("domain_max_g", 1.0) as f32,
        params.get_f64_or("domain_max_b", 1.0) as f32,
    ];
    match Lut3D::from_raw(size, data, domain_min, domain_max) {
        Some(lut) => (lut, amount),
        None => (fallback_identity_lut(), amount),
    }
}

/// Applies the creative LUT to one working-space RGBA pixel (alpha
/// untouched): `companion_encode` → tetrahedral sample → amount blend
/// (interpolate `[0,1]` / extrapolate `(1,2]`, clamped in-gamut) →
/// `companion_decode`, the CPU parity anchor for
/// `global_creative_lut.wgsl`'s `main` entry.
fn apply_creative_lut(rgba: [f32; 4], lut: &Lut3D, amount: f32) -> [f32; 4] {
    let enc = companion_encode([rgba[0], rgba[1], rgba[2]]);
    let mapped = lut.sample_tetrahedral(enc);
    let blended = if amount <= 1.0 {
        let k = amount.clamp(0.0, 1.0);
        [
            enc[0] + k * (mapped[0] - enc[0]),
            enc[1] + k * (mapped[1] - enc[1]),
            enc[2] + k * (mapped[2] - enc[2]),
        ]
    } else {
        let extra = amount - 1.0;
        [
            mapped[0] + extra * (mapped[0] - enc[0]),
            mapped[1] + extra * (mapped[1] - enc[1]),
            mapped[2] + extra * (mapped[2] - enc[2]),
        ]
    };
    let clamped = [
        blended[0].clamp(0.0, 1.0),
        blended[1].clamp(0.0, 1.0),
        blended[2].clamp(0.0, 1.0),
    ];
    let dec = companion_decode(clamped);
    [dec[0], dec[1], dec[2], rgba[3]]
}

/// The `global.creative_lut` develop node.
#[derive(Default)]
pub struct CreativeLutNode {}

impl CreativeLutNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.creative_lut");

    /// A fresh node.
    pub fn new() -> CreativeLutNode {
        CreativeLutNode::default()
    }

    /// Encodes `lut` + `amount` (the node's own `0.0..=2.0` fraction domain
    /// `0` identity, `1` full strength, up to `2` extrapolated) into a
    /// [`ParamBlock`] `eval_cpu`/`eval_gpu` consume directly, the D10-seam
    /// test/harness constructor (module doc): the exact call D10's real
    /// `id`-resolution wiring will make once it exists, and the call this
    /// phase's own tests use to prove the node's math end to end without a
    /// catalog DB.
    pub fn param_block_with_lut(amount: f32, lut: &Lut3D) -> ParamBlock {
        let (dmin, dmax) = lut.domain();
        ParamBlock::from_fields([
            ("amount", ParamValue::Float(amount as f64)),
            ("look_id", ParamValue::Text(String::new())),
            ("lut_size", ParamValue::Int(i64::from(lut.size()))),
            ("lut_data", ParamValue::Text(encode_lut_data(lut))),
            ("domain_min_r", ParamValue::Float(dmin[0] as f64)),
            ("domain_min_g", ParamValue::Float(dmin[1] as f64)),
            ("domain_min_b", ParamValue::Float(dmin[2] as f64)),
            ("domain_max_r", ParamValue::Float(dmax[0] as f64)),
            ("domain_max_g", ParamValue::Float(dmax[1] as f64)),
            ("domain_max_b", ParamValue::Float(dmax[2] as f64)),
        ])
        .expect("a Lut3D's own data/domain are always finite by construction")
    }
}

impl RenderNode for CreativeLutNode {
    fn descriptor(&self) -> &NodeDescriptor {
        &DESCRIPTOR
    }

    // Pointwise (a per-pixel 3D-LUT sample needs no spatial neighborhood),
    // so the default `RenderNode::plan`/`GlobalNode::roi_in` (`out` in ==
    // `out` out) is correct as-is, no override needed.

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
            .ok_or_else(|| NodeError::Other("global.creative_lut: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.creative_lut: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let (lut, amount) = resolve_from_params(params);
        let (dmin, dmax) = lut.domain();
        let n = lut.size();

        // Params UBO: matches `global_creative_lut.wgsl`'s `Params` layout
        // exactly (amount, size, 2 pad floats, domain_min vec4, domain_max
        // vec4, 48 bytes, natural WGSL alignment).
        let mut ubo_bytes = [0u8; 48];
        ubo_bytes[0..4].copy_from_slice(&amount.to_le_bytes());
        ubo_bytes[4..8].copy_from_slice(&(n as f32).to_le_bytes());
        ubo_bytes[16..20].copy_from_slice(&dmin[0].to_le_bytes());
        ubo_bytes[20..24].copy_from_slice(&dmin[1].to_le_bytes());
        ubo_bytes[24..28].copy_from_slice(&dmin[2].to_le_bytes());
        ubo_bytes[32..36].copy_from_slice(&dmax[0].to_le_bytes());
        ubo_bytes[36..40].copy_from_slice(&dmax[1].to_le_bytes());
        ubo_bytes[40..44].copy_from_slice(&dmax[2].to_le_bytes());
        let ubo = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("global.creative_lut params"),
            size: ubo_bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&ubo, 0, &ubo_bytes);

        // The 3D LUT texture: `Lut3D::to_padded_rgba_f32` is ALREADY in
        // wgpu's expected x-fastest/y/z-slowest linear layout (r->x, g->y,
        // b->z), so the upload is a single `write_texture` with no reorder.
        let lut_tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("global.creative_lut table"),
            size: wgpu::Extent3d {
                width: n,
                height: n,
                depth_or_array_layers: n,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut_bytes: Vec<u8> = lut
            .to_padded_rgba_f32()
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &lut_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &lut_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(n * 16), // rgba32float = 16 bytes/texel
                rows_per_image: Some(n),
            },
            wgpu::Extent3d {
                width: n,
                height: n,
                depth_or_array_layers: n,
            },
        );
        let lut_view = lut_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let pipeline = ctx.kernels.compute_pipeline(CREATIVE_LUT_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.creative_lut in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.creative_lut out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_params = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.creative_lut params"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubo.as_entire_binding(),
            }],
        });
        let bg_lut = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.creative_lut lut"),
            layout: &pipeline.get_bind_group_layout(3),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&lut_view),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_params, &bg_lut],
            extent,
            "global.creative_lut",
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
            return Err(NodeError::Cpu(
                "global.creative_lut: missing input tile".into(),
            ));
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.creative_lut: missing input tile".into()))?
            .pixels;
        let (lut, amount) = resolve_from_params(params);
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_creative_lut(p, &lut, amount);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for CreativeLutNode {
    fn is_identity(p: &GlobalStages) -> bool {
        match &p.effects.creative_lut {
            None => true,
            Some(l) => l.amount <= 0.0,
        }
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        match &p.effects.creative_lut {
            None => ParamBlock::from_fields([
                ("amount", ParamValue::Float(0.0)),
                ("lut_size", ParamValue::Int(0)),
            ])
            .expect("empty creative-lut params always build"),
            Some(l) => {
                // Node-domain amount is a 0..=2 fraction (spec §4.8); the
                // recipe's own field is a 0..=100 percentage with unity=100
                // (`lightbox_edit::leaves::CreativeLut` doc comment), see
                // this module's doc for why the percentage range tops out at
                // 100 (no >100% extrapolation reachable from the recipe
                // today) while the node's own math supports up to 200%
                // (exercised directly via `param_block_with_lut` in tests).
                let amount = (l.amount as f64 / 100.0).clamp(0.0, 2.0);
                ParamBlock::from_fields([
                    ("amount", ParamValue::Float(amount)),
                    ("look_id", ParamValue::Text(l.id.clone())),
                    ("lut_size", ParamValue::Int(0)),
                ])
                .expect("creative-lut fields are always finite (clamped on ingest — spec A2)")
            }
        }
    }
}

/// Factory registering [`CreativeLutNode`].
#[derive(Default)]
pub struct CreativeLutFactory {}

impl NodeFactory for CreativeLutFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(CreativeLutNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(CREATIVE_LUT_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::leaves::CreativeLut;

    #[test]
    fn is_identity_tracks_presence_and_amount() {
        let mut g = GlobalStages::default();
        assert!(CreativeLutNode::is_identity(&g));

        g.effects.creative_lut = Some(CreativeLut {
            id: "some-look".to_owned(),
            amount: 100.0,
        });
        assert!(!CreativeLutNode::is_identity(&g));

        g.effects.creative_lut = Some(CreativeLut {
            id: "some-look".to_owned(),
            amount: 0.0,
        });
        assert!(CreativeLutNode::is_identity(&g), "amount 0 is identity");
    }

    #[test]
    fn param_block_converts_percent_to_fraction() {
        let g = GlobalStages {
            effects: lightbox_edit::leaves::Effects {
                creative_lut: Some(CreativeLut {
                    id: "look-x".to_owned(),
                    amount: 40.0,
                }),
                ..Default::default()
            },
            ..GlobalStages::default()
        };
        let pb = CreativeLutNode::param_block(&g);
        assert!((pb.get_f64("amount").unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(pb.get_str("look_id"), Some("look-x"));
    }

    /// D10: `recipe_amount_to_fraction` (the resolver-wiring call site's
    /// helper) agrees exactly with `param_block`'s own internal conversion
    /// proven by comparing both against the same inputs `param_block_
    /// converts_percent_to_fraction` already exercises, plus the 0/100/200
    /// clamp boundaries.
    #[test]
    fn recipe_amount_to_fraction_matches_param_blocks_own_conversion() {
        assert!((recipe_amount_to_fraction(40.0) - 0.4).abs() < 1e-9);
        assert!((recipe_amount_to_fraction(0.0) - 0.0).abs() < 1e-9);
        assert!((recipe_amount_to_fraction(100.0) - 1.0).abs() < 1e-9);
        assert!((recipe_amount_to_fraction(200.0) - 2.0).abs() < 1e-9);
        // Out-of-domain inputs clamp rather than producing an out-of-range
        // node-domain amount (defensive, a future caller should never be
        // able to smuggle an unclamped value through this seam).
        assert!((recipe_amount_to_fraction(-50.0) - 0.0).abs() < 1e-9);
        assert!((recipe_amount_to_fraction(500.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn no_resolver_wired_yet_degrades_to_identity() {
        // The production `param_block` path (from a real GlobalStages) never
        // carries table data (no D10 resolver), resolve_from_params must
        // therefore treat it as the fallback identity LUT regardless of
        // amount, per the module doc's graceful-degrade contract.
        let g = GlobalStages {
            effects: lightbox_edit::leaves::Effects {
                creative_lut: Some(CreativeLut {
                    id: "unresolvable-hash".to_owned(),
                    amount: 100.0,
                }),
                ..Default::default()
            },
            ..GlobalStages::default()
        };
        let pb = CreativeLutNode::param_block(&g);
        let (lut, amount) = resolve_from_params(&pb);
        assert!((amount - 1.0).abs() < 1e-6);
        assert!(lut.is_identity(1e-6));
        let out = apply_creative_lut([0.2, 0.5, 0.9, 1.0], &lut, amount);
        assert!((out[0] - 0.2).abs() < 1e-4);
        assert!((out[1] - 0.5).abs() < 1e-4);
        assert!((out[2] - 0.9).abs() < 1e-4);
    }

    // ── D9: identity / amount / known-mapping proofs at the pure-math level
    //    (the direct "param/test harness" seam `param_block_with_lut` gives) ──

    #[test]
    fn identity_lut_is_passthrough_at_full_amount() {
        let lut = Lut3D::identity(5);
        for amount in [0.0f32, 0.5, 1.0, 1.5, 2.0] {
            for rgba in [
                [0.1f32, 0.4, 0.8, 1.0],
                [0.0, 0.0, 0.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
            ] {
                let out = apply_creative_lut(rgba, &lut, amount);
                for (c, (&o, &i)) in out.iter().zip(rgba.iter()).enumerate().take(3) {
                    assert!(
                        (o - i).abs() < 1.0e-4,
                        "identity LUT amount={amount}: in={rgba:?} out={out:?} (channel {c})"
                    );
                }
            }
        }
    }

    #[test]
    fn amount_zero_is_identity_regardless_of_lut_content() {
        let lut = Lut3D::bake(4, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
        let rgba = [0.2f32, 0.6, 0.9, 1.0];
        let out = apply_creative_lut(rgba, &lut, 0.0);
        for (o, i) in out.iter().zip(rgba.iter()).take(3) {
            assert!((o - i).abs() < 1.0e-4, "out={out:?}");
        }
    }

    #[test]
    fn known_nonidentity_lut_produces_the_expected_mapping_at_full_amount() {
        // A known LUT: swap R and G (companion-domain).
        let lut = Lut3D::bake(5, |[r, g, b]| [g, r, b]);
        let rgba = [0.8f32, 0.2, 0.5, 1.0];
        let out = apply_creative_lut(rgba, &lut, 1.0);
        // Companion-encode/decode round-trips through the swap; expect
        // approximately the R/G-swapped color back in working space.
        let enc = companion_encode([rgba[0], rgba[1], rgba[2]]);
        let want_enc = [enc[1], enc[0], enc[2]];
        let want = companion_decode(want_enc);
        for (c, (&o, &w)) in out.iter().zip(want.iter()).enumerate().take(3) {
            assert!(
                (o - w).abs() < 0.02,
                "channel {c}: out={out:?} want={want:?}"
            );
        }
    }

    #[test]
    fn amount_two_extrapolates_and_clamps_in_gamut() {
        // A LUT that pushes the encoded value strongly toward 1.0 for any
        // input above 0, at amount=1 it already reaches near the top of
        // the range; pure linear extrapolation to amount=2 would overshoot
        // the encoded [0,1] range, which must clamp rather than produce an
        // out-of-gamut (or non-finite) result.
        let lut = Lut3D::bake(3, |[r, g, b]| {
            [(r + 0.7).min(1.0), (g + 0.7).min(1.0), (b + 0.7).min(1.0)]
        });
        let rgba = [0.5f32, 0.5, 0.5, 1.0];
        let at_full = apply_creative_lut(rgba, &lut, 1.0);
        let at_double = apply_creative_lut(rgba, &lut, 2.0);
        for (c, &v) in at_double.iter().enumerate().take(3) {
            assert!(v.is_finite(), "channel {c} must stay finite");
            // In-gamut clamp: the decoded (linear) value can never exceed
            // companion_decode(1.0) == 1.0.
            assert!(v <= 1.0 + 1e-4, "channel {c}={v} must clamp in-gamut");
        }
        // Extrapolation must push at least as far as full-strength did (the
        // LUT keeps pushing toward 1.0, so 2.0 should be >= 1.0's result).
        for (c, (&full, &double)) in at_full.iter().zip(at_double.iter()).enumerate().take(3) {
            assert!(
                double >= full - 1e-4,
                "channel {c}: extrapolation should not move backward: full={full} double={double}"
            );
        }
    }

    #[test]
    fn param_block_with_lut_round_trips_through_resolve() {
        let lut = Lut3D::bake(4, |[r, g, b]| [b, r, g]);
        let pb = CreativeLutNode::param_block_with_lut(0.75, &lut);
        let (resolved, amount) = resolve_from_params(&pb);
        assert!((amount - 0.75).abs() < 1e-6);
        assert_eq!(resolved.size(), lut.size());
        for (a, b) in resolved.data().iter().zip(lut.data().iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn malformed_lut_data_in_params_degrades_to_identity_not_panic() {
        // Hand-construct a ParamBlock with a size/data mismatch (as if a
        // future producer got confused), must degrade gracefully, never
        // index out of bounds or panic.
        let pb = ParamBlock::from_fields([
            ("amount", ParamValue::Float(1.0)),
            ("lut_size", ParamValue::Int(5)),
            (
                "lut_data",
                ParamValue::Text("garbage;not,numbers".to_owned()),
            ),
        ])
        .unwrap();
        let (lut, amount) = resolve_from_params(&pb);
        assert!((amount - 1.0).abs() < 1e-6);
        assert!(lut.is_identity(1e-6));
    }
}
