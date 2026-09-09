// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `global.bw_mix` (E10 task **D1**). Band-weighted black-and-white
//! conversion: an 8-slider channel mixer (Red…Magenta, the same
//! Lightroom-named hue bands the HSL mixer and color grading use) that
//! decides how much each hue band contributes to the output gray level,
//! plus [`auto_bw_mix`], a deterministic, white-balance-informed initial
//! mix (task D1's `AutoBwMix`).
//!
//! # The per-pixel remap
//!
//! For a working-space pixel: take its working luma `L` (the plain
//! Rec.-weighted gray conversion,
//! [`crate::ng::nodes::global::tone_recovery::working_luma`], the "neutral
//! luma conversion" the D1 AC anchors the all-zero mix to) and its OkLCh
//! chroma/hue `(C, h)`. Compute the 8 [`colorspace::band_weights`] at `h`
//! the **shared C7 weights** this node reuses rather than re-deriving (the
//! same partition-of-unity construction `HslNode`/`ColorGradeNode` already
//! use), and take their weighted sum against the 8 mix sliders to get a
//! single scalar `boost ∈ [-1, 1]` (each slider is `-100..=100`). The output
//! gray level is `L * gain`, where `gain = max(1 + boost · MIX_CHROMA_SCALE ·
//! C, 0)`.
//!
//! **Why the chroma factor matters (the D1 AC's "mix [0;8] ≡ neutral luma
//! conversion" property, generalized).** Multiplying `boost` by the pixel's
//! own chroma `C` means a genuinely achromatic pixel (`C == 0`, no hue to
//! attribute to any band) is **always** rendered as its plain luma,
//! regardless of the mix sliders, not merely when the mix happens to be
//! all-zero. This is the physically sensible reading of "how much do reds
//! contribute to gray": a pixel that isn't red (or anything else) can't be
//! moved by a slider that only reweights hue bands. At `mix == [0; 8]`,
//! `boost == 0` for every pixel (chromatic or not) so `gain == 1`
//! everywhere and the node is the *exact* identity vs. plain luma, the D1
//! AC holds as a special case of this stronger, always-true property
//! ([`tests::neutral_pixel_is_never_moved_by_any_mix`]).
//!
//! Output is fully desaturated (`[gray, gray, gray, alpha]`), the whole
//! point of a monochrome conversion.
//!
//! GPU (`shaders/global_bw_mix.wgsl`) and CPU ([`apply_bw_mix`]) evaluate the
//! identical formula from the identical baked matrices/geometry/mix data (the
//! same "bake once, upload verbatim" pattern `global_hsl.wgsl` uses), so
//! CPU/GPU parity (§4.4) holds to float rounding.
//!
//! # Elision: `Treatment::Color` only
//!
//! Unlike every prior Phase A-C node (whose identity is "every field at its
//! neutral default"), [`GlobalNode::is_identity`] here is **not** "mix is all
//! zero", a `BlackAndWhite` treatment with an all-zero mix still desaturates
//! the image (a real, non-identity transform: `[r,g,b] → [L,L,L]` for a
//! chromatic pixel). The node is elided **only** when `treatment ==
//! Treatment::Color`, i.e. B&W is off entirely. This is spec §4.1's
//! "Treatment toggle must elide downstream color nodes when Monochrome"
//! read the other direction for this specific node: `BwMixNode` itself is
//! elided in `Color` mode (task D2's own downstream-elision half, the
//! color-mixer nodes upstream/parallel to it, is a **shell** concern
//! (`crates/lightbox-shell`), out of this phase agent's scope; this module's
//! own graph-probe test proves `global.bw_mix` never appears for a `Color`
//! recipe, the render-engine half of that contract).

use std::sync::Arc;

use lightbox_edit::{BwMix, GlobalStages, Treatment, WhiteBalance};

use crate::ng::error::NodeError;
use crate::ng::gpu::dispatch_compute;
use crate::ng::node::param::{FieldDecl, ParamKind, ParamsSchema, ParamsSchemaRef};
use crate::ng::node::{
    CpuEvalCtx, GpuEvalCtx, KernelSalt, NodeDescriptor, NodeFactory, ParamBlock, ParamValue,
    PortDecl, RenderNode,
};
use crate::ng::nodes::common::colorspace;
use crate::ng::nodes::global::tone_recovery::{working_luma, working_luma_weights};
use crate::ng::nodes::global::white_balance::resolve_temp_tint;
use crate::ng::nodes::global::GlobalNode;
use crate::ng::tile::{CpuTileView, PixelBuf, TileView};
use crate::ng::types::{Extent, NodeId, PortType};

/// The B&W mix kernel, naga-validated at build (`build.rs`).
const BW_MIX_WGSL: &str = include_str!("../../../../shaders/global_bw_mix.wgsl");

/// Chroma-gated slider-to-gain calibration (spec's "implementer picks the
/// slider-to-physical-unit mapping" latitude, the same modeling freedom
/// `hsl::SAT_RANGE`/`whites_blacks::ENDPOINT_RANGE` take): a fully-deflected
/// (`±100`) band at a pixel of Oklab chroma `C` shifts that pixel's gray gain
/// by up to `± MIX_CHROMA_SCALE · C`. Kept in exact sync with the WGSL
/// constant of the same name.
pub const MIX_CHROMA_SCALE: f32 = 1.0;

fn mix_field(i: usize) -> &'static str {
    const NAMES: [&str; 8] = ["m0", "m1", "m2", "m3", "m4", "m5", "m6", "m7"];
    NAMES[i]
}

static SCHEMA: ParamsSchema = ParamsSchema::new(&[
    FieldDecl {
        name: "m0",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m1",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m2",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m3",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m4",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m5",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m6",
        kind: ParamKind::Float,
    },
    FieldDecl {
        name: "m7",
        kind: ParamKind::Float,
    },
]);

static DESCRIPTOR: NodeDescriptor = NodeDescriptor {
    id: NodeId("global.bw_mix"),
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

/// Reads the 8 mix-slider fields out of a [`ParamBlock`] built by
/// [`BwMixNode::param_block`].
fn mix_from_params(params: &ParamBlock) -> [f32; 8] {
    let mut mix = [0f32; 8];
    for (i, m) in mix.iter_mut().enumerate() {
        *m = params.get_f64_or(mix_field(i), 0.0) as f32;
    }
    mix
}

/// Applies the band-weighted B&W mix to one working-space RGBA pixel, the
/// CPU parity anchor for `global_bw_mix.wgsl`. See the module docs for the
/// formula and its "achromatic pixels are never moved" property.
pub fn apply_bw_mix(rgba: [f32; 4], mix: &[f32; 8], luma_weights: [f32; 3]) -> [f32; 4] {
    let luma = working_luma([rgba[0], rgba[1], rgba[2]], luma_weights);
    let [_, c, h] = colorspace::working_to_oklch([rgba[0], rgba[1], rgba[2]]);
    let w = colorspace::band_weights(h);
    let mut boost = 0.0f32;
    for i in 0..8 {
        boost += w[i] * (mix[i] / 100.0);
    }
    let gain = (1.0 + boost * MIX_CHROMA_SCALE * c).max(0.0);
    let gray = (luma * gain).max(0.0);
    [gray, gray, gray, rgba[3]]
}

// ── AutoBwMix (task D1) ──────────────────────────────────────────────────

/// A documented, fixed "classic contrast" starting mix (approximates a
/// classic red-filter monochrome conversion, warm bands boosted, cool bands
/// cut, punchier skies and skin than a plain luma conversion), in
/// [`colorspace::HUE_BAND_NAMES`] order (Red, Orange, Yellow, Green, Aqua,
/// Blue, Purple, Magenta).
const AUTO_BASE_MIX: [f32; 8] = [40.0, 20.0, 0.0, -20.0, -20.0, -40.0, -20.0, 10.0];

/// Warm/cool sign per band: `+1` = a warm band (pulled further from its base
/// by a warm/tungsten-ish white balance), `-1` = a cool band, `±0.5` for the
/// two bands that sit between warm and cool. Same order as
/// [`AUTO_BASE_MIX`].
const AUTO_WARM_SIGN: [f32; 8] = [1.0, 1.0, 0.5, -0.5, -1.0, -1.0, -0.5, 0.5];

/// The neutral-reference Kelvin `warmth` is measured against (roughly
/// daylight/D65-ish), chosen so `warmth == 0` at a common outdoor WB, not at
/// an arbitrary CCT.
const AUTO_WB_REF_K: f32 = 6500.0;
/// The Kelvin span that saturates `warmth` to `±1` (tungsten ⇒ +1 at
/// `AUTO_WB_REF_K - AUTO_WB_SPAN_K ≈ 3000K`; a cool shade WB ⇒ -1 at
/// `AUTO_WB_REF_K + AUTO_WB_SPAN_K ≈ 10000K`).
const AUTO_WB_SPAN_K: f32 = 3500.0;
/// How many mix points a fully-saturated `warmth` nudges each band, atop its
/// [`AUTO_BASE_MIX`] starting point.
const AUTO_WB_GAIN: f32 = 15.0;

/// **Task D1's `AutoBwMix`**: a documented, deterministic, white-balance-
/// informed initial B&W mix.
///
/// # The formula
///
/// Starts from the fixed [`AUTO_BASE_MIX`] "classic contrast" mix and nudges
/// it by `wb`'s resolved correlated color temperature: a warm (tungsten-ish,
/// low Kelvin) white balance means the scene light itself already skews
/// warm, so this pulls the warm bands' contribution back down and pushes the
/// cool bands up (and the mirror image for a cool/shade white balance), the
/// intent is a B&W conversion whose apparent contrast isn't simply an
/// artifact of the color temperature the shot happened to be taken under.
///
/// `temp_k` resolves through the exact same
/// [`white_balance::resolve_temp_tint`](resolve_temp_tint) `AsShot`/`Auto ⇒
/// None ⇒ AUTO_WB_REF_K` fallback [`WhiteBalanceNode`](super::WhiteBalanceNode)
/// itself uses, so this is a pure, deterministic function of `wb` for
/// **every** `WhiteBalance` value, including the ones with no real CCT
/// solve yet (spec D1 AC "auto produces a documented deterministic result
/// per WB").
///
/// This is the render/engine-layer half of D1's `AutoBwMix` command (the
/// pure algorithm); wiring it onto `lightbox-core`'s command bus as a
/// user-facing action is out of this phase agent's `lightbox-render`-only
/// scope (`docs/plan/epics/E10-deviations.md`).
#[must_use]
pub fn auto_bw_mix(wb: &WhiteBalance) -> BwMix {
    let temp_k = resolve_temp_tint(wb)
        .map(|(k, _)| k as f32)
        .unwrap_or(AUTO_WB_REF_K);
    let warmth = ((AUTO_WB_REF_K - temp_k) / AUTO_WB_SPAN_K).clamp(-1.0, 1.0);
    let mut weights = [0f32; 8];
    for i in 0..8 {
        // A warm WB (warmth > 0) pulls warm bands (positive AUTO_WARM_SIGN)
        // DOWN and cool bands (negative AUTO_WARM_SIGN) UP, hence the
        // subtraction (see the fn doc's "compensate for scene warmth").
        weights[i] =
            (AUTO_BASE_MIX[i] - warmth * AUTO_WARM_SIGN[i] * AUTO_WB_GAIN).clamp(-100.0, 100.0);
    }
    BwMix { weights }
}

/// The `global.bw_mix` develop node.
#[derive(Default)]
pub struct BwMixNode {}

impl BwMixNode {
    /// The node's registry identity (spec §4.4 NodeId inventory).
    pub const ID: NodeId = NodeId("global.bw_mix");

    /// A fresh node.
    pub fn new() -> BwMixNode {
        BwMixNode::default()
    }
}

impl RenderNode for BwMixNode {
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
            .ok_or_else(|| NodeError::Other("global.bw_mix: missing input tile".into()))?;
        let out_view = ctx
            .output()
            .texture_view()
            .ok_or_else(|| NodeError::Gpu("global.bw_mix: output is not a GPU tile".into()))?
            .clone();
        let extent = ctx.output().extent().unwrap_or(Extent {
            w: input.roi.w,
            h: input.roi.h,
        });

        let mix = mix_from_params(params);
        let weights = working_luma_weights();
        let mut data = Vec::with_capacity(63);
        data.extend_from_slice(&weights);
        data.extend_from_slice(&colorspace::matrices_flat());
        data.extend_from_slice(&colorspace::hue_band_geometry_flat());
        data.extend_from_slice(&mix);
        let buf = storage_buffer(ctx.device, ctx.queue, "global.bw_mix data", &data);

        let pipeline = ctx.kernels.compute_pipeline(BW_MIX_WGSL, "main")?;
        let bg_in = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.bw_mix in"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(input.view),
            }],
        });
        let bg_out = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.bw_mix out"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&out_view),
            }],
        });
        let bg_data = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("global.bw_mix data"),
            layout: &pipeline.get_bind_group_layout(2),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        });
        dispatch_compute(
            ctx.device,
            ctx.queue,
            &pipeline,
            &[&bg_in, &bg_out, &bg_data],
            extent,
            "global.bw_mix",
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
            return Err(NodeError::Cpu("global.bw_mix: missing input tile".into()));
        }
        let input = inputs
            .first()
            .ok_or_else(|| NodeError::Cpu("global.bw_mix: missing input tile".into()))?
            .pixels;
        let mix = mix_from_params(params);
        let weights = working_luma_weights();
        let out = ctx.output();
        let (w, fmt, bpp) = (
            out.extent.w,
            out.format,
            out.format.bytes_per_pixel() as usize,
        );
        out.par_fill_rows(|y, row| {
            for x in 0..w {
                let p = input.get_rgba_f32(x, y);
                let o = apply_bw_mix(p, &mix, weights);
                PixelBuf::encode_pixel(fmt, &mut row[x as usize * bpp..], o);
            }
        });
        Ok(())
    }
}

impl GlobalNode for BwMixNode {
    fn is_identity(p: &GlobalStages) -> bool {
        // Elided iff Color treatment (see the module docs, NOT "mix is
        // all-zero"; an all-zero mix in Monochrome still desaturates).
        p.treatment != Treatment::BlackAndWhite
    }

    fn param_block(p: &GlobalStages) -> ParamBlock {
        let mut fields: Vec<(&'static str, ParamValue)> = Vec::with_capacity(8);
        for (i, w) in p.bw.weights.iter().enumerate() {
            fields.push((mix_field(i), ParamValue::Float(*w as f64)));
        }
        ParamBlock::from_fields(fields)
            .expect("bw mix weights are always finite (clamped on ingest — spec A2)")
    }
}

/// A GPU storage buffer uploaded with `samples`' `f32` values, mirrors
/// `nodes::global::hsl`'s `storage_buffer` helper.
fn storage_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    samples: &[f32],
) -> wgpu::Buffer {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buf, 0, &bytes);
    buf
}

/// Factory registering [`BwMixNode`].
#[derive(Default)]
pub struct BwMixFactory {}

impl NodeFactory for BwMixFactory {
    fn instantiate(&self) -> Arc<dyn RenderNode> {
        Arc::new(BwMixNode::new())
    }

    fn kernel_salt(&self) -> KernelSalt {
        KernelSalt(blake3::hash(BW_MIX_WGSL.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::WbPreset;

    fn neutral_mix() -> [f32; 8] {
        [0.0; 8]
    }

    // ── D1 AC: mix [0;8] ≡ neutral luma conversion ────────────────────────

    #[test]
    fn all_zero_mix_equals_plain_luma_conversion() {
        let weights = working_luma_weights();
        let mix = neutral_mix();
        for rgba in [
            [0.8, 0.2, 0.1, 1.0],
            [0.1, 0.9, 0.3, 0.5],
            [0.02, 0.4, 0.95, 1.0],
            [1.3, 0.6, 0.05, 1.0], // scene-linear highlight, > 1
        ] {
            let out = apply_bw_mix(rgba, &mix, weights);
            let want = working_luma([rgba[0], rgba[1], rgba[2]], weights);
            assert!((out[0] - want).abs() < 1e-4, "rgba={rgba:?} out={out:?}");
            assert_eq!(out[0], out[1]);
            assert_eq!(out[1], out[2]);
            assert_eq!(out[3], rgba[3]);
        }
    }

    /// The stronger property the module docs claim: an achromatic (gray)
    /// pixel is unaffected by ANY mix setting, not just the all-zero one.
    #[test]
    fn neutral_pixel_is_never_moved_by_any_mix() {
        let weights = working_luma_weights();
        let gray = [0.35f32, 0.35, 0.35, 1.0];
        let want = working_luma([gray[0], gray[1], gray[2]], weights);
        for mix in [
            [100.0, -100.0, 50.0, -50.0, 100.0, -100.0, 20.0, -20.0],
            [-100.0; 8],
            [100.0; 8],
        ] {
            let out = apply_bw_mix(gray, &mix, weights);
            assert!((out[0] - want).abs() < 1e-4, "mix={mix:?} out={out:?}");
        }
    }

    /// A saturated red pixel IS moved by the Red band's slider (a real,
    /// visible pixel change, the node actually does something for a
    /// chromatic pixel).
    #[test]
    fn a_saturated_pixel_is_moved_by_its_own_band() {
        let weights = working_luma_weights();
        let red = [1.0f32, 0.0, 0.0, 1.0];
        let neutral_out = apply_bw_mix(red, &neutral_mix(), weights);
        let mut boosted = neutral_mix();
        boosted[0] = 100.0; // Red band, max
        let boosted_out = apply_bw_mix(red, &boosted, weights);
        assert!(
            boosted_out[0] > neutral_out[0] + 1e-3,
            "boosting the Red band must brighten a red pixel's gray output: \
             neutral={neutral_out:?} boosted={boosted_out:?}"
        );
    }

    #[test]
    fn output_is_always_fully_desaturated() {
        let weights = working_luma_weights();
        let mut mix = neutral_mix();
        mix[3] = 80.0;
        let out = apply_bw_mix([0.2, 0.7, 0.4, 1.0], &mix, weights);
        assert_eq!(out[0], out[1]);
        assert_eq!(out[1], out[2]);
    }

    // ── node wiring / elision ──────────────────────────────────────────────

    #[test]
    fn color_treatment_is_always_identity_regardless_of_mix() {
        let mut g = GlobalStages {
            treatment: Treatment::Color,
            ..GlobalStages::default()
        };
        assert!(BwMixNode::is_identity(&g));
        g.bw.weights[0] = 90.0;
        assert!(
            BwMixNode::is_identity(&g),
            "Color treatment elides bw_mix even with a non-zero mix"
        );
    }

    #[test]
    fn monochrome_treatment_is_never_identity_even_with_zero_mix() {
        let g = GlobalStages {
            treatment: Treatment::BlackAndWhite,
            ..GlobalStages::default()
        };
        assert!(
            !BwMixNode::is_identity(&g),
            "Monochrome must never be elided — it always desaturates"
        );
    }

    #[test]
    fn param_block_round_trips_mix_weights() {
        let mut g = GlobalStages {
            treatment: Treatment::BlackAndWhite,
            ..GlobalStages::default()
        };
        g.bw.weights = [10.0, -20.0, 30.0, -40.0, 50.0, -60.0, 70.0, -80.0];
        let pb = BwMixNode::param_block(&g);
        for (i, &want) in g.bw.weights.iter().enumerate() {
            assert_eq!(pb.get_f64(mix_field(i)), Some(want as f64));
        }
    }

    // ── AutoBwMix (D1) ──────────────────────────────────────────────────────

    #[test]
    fn auto_bw_mix_is_deterministic() {
        let a = auto_bw_mix(&WhiteBalance::Custom {
            temp_k: 3200.0,
            tint: 0.0,
        });
        let b = auto_bw_mix(&WhiteBalance::Custom {
            temp_k: 3200.0,
            tint: 0.0,
        });
        assert_eq!(a.weights, b.weights);
    }

    #[test]
    fn auto_bw_mix_resolves_as_shot_and_auto_to_the_same_reference() {
        let as_shot = auto_bw_mix(&WhiteBalance::AsShot);
        let auto = auto_bw_mix(&WhiteBalance::Auto);
        assert_eq!(as_shot.weights, auto.weights);
    }

    #[test]
    fn auto_bw_mix_all_weights_stay_in_range() {
        for wb in [
            WhiteBalance::Custom {
                temp_k: 2000.0,
                tint: 0.0,
            },
            WhiteBalance::Custom {
                temp_k: 50000.0,
                tint: 0.0,
            },
            WhiteBalance::Preset(WbPreset::Tungsten),
            WhiteBalance::Preset(WbPreset::Shade),
        ] {
            let mix = auto_bw_mix(&wb);
            for w in mix.weights {
                assert!((-100.0..=100.0).contains(&w), "weight {w} out of range");
            }
        }
    }

    /// A warm (tungsten, low Kelvin) WB pulls the Red band's contribution
    /// DOWN relative to a cool (shade, high Kelvin) WB, the "compensate for
    /// scene warmth" property the module docs describe.
    #[test]
    fn warmer_white_balance_reduces_the_warm_bands_relative_to_cooler() {
        let warm = auto_bw_mix(&WhiteBalance::Custom {
            temp_k: 3000.0,
            tint: 0.0,
        });
        let cool = auto_bw_mix(&WhiteBalance::Custom {
            temp_k: 10000.0,
            tint: 0.0,
        });
        assert!(
            warm.weights[0] < cool.weights[0],
            "warm-WB Red band ({}) must be lower than cool-WB Red band ({})",
            warm.weights[0],
            cool.weights[0]
        );
        // And the mirror on a cool band (Blue, index 5): warm WB pushes it
        // up relative to cool WB.
        assert!(
            warm.weights[5] > cool.weights[5],
            "warm-WB Blue band ({}) must be higher than cool-WB Blue band ({})",
            warm.weights[5],
            cool.weights[5]
        );
    }
}
