// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! HueSat LUT evaluator, shared by the DCP path and the Lightbox look
//! (spec §3.4). **Owner: Phase B (B6).** Trilinear with hue wrap, Linear/sRGB
//! encodings, `val-dims == 1` fast path.

use crate::matrix::spaces::srgb_oetf;

/// The encoding a HueSat/Look table's saturation/value axes use (DNG
/// `ProfileHueSatMapEncoding` / `ProfileLookTableEncoding`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum HueSatEncoding {
    /// Linear encoding.
    Linear,
    /// sRGB encoding.
    Srgb,
}

/// A HueSatMap / LookTable delta cube (spec §3.4). `deltas` is
/// `dims[0] * dims[1] * dims[2]` entries of `[Δhue°, Δsat×, Δval×]`, indexed
/// hue-major.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HueSatLut {
    /// `[hue_divisions, sat_divisions, val_divisions]`.
    pub dims: [u32; 3],
    /// Per-node deltas.
    pub deltas: Vec<[f32; 3]>,
    /// Axis encoding.
    pub encoding: HueSatEncoding,
}

impl HueSatLut {
    /// Applies the table to an `HSV` sample (spec §3.4). **Phase B (B6)**
    /// trilinear, hue-wrapped (no seam at 0°/360°), `val-dims == 1` fast path.
    ///
    /// `hsv` is `[hue°, saturation, value]` with `hue ∈ [0, 360)`, `sat ∈ [0, 1]`,
    /// `value ∈ [0, 1]`. Returns the shaped `[hue°, sat, value]`.
    pub fn eval(&self, hsv: [f32; 3]) -> [f32; 3] {
        let delta = interpolate_delta(self.dims, &self.deltas, hsv, self.encoding);
        apply_delta(hsv, delta)
    }
}

/// A HueSat table resolved for GPU upload (the `ResolvedInputTransform` form,
/// spec §3.4). Carries its value-axis `encoding` so a resolved eval indexes on
/// the same coordinate the DNG semantics use.
#[derive(Clone, Debug)]
pub struct HueSatTable {
    /// `[hue_divisions, sat_divisions, val_divisions]`.
    pub dims: [u32; 3],
    /// Deltas in hue-major (`(h·sat + s)·val + v`) order, upload-ready.
    pub deltas: Vec<[f32; 3]>,
    /// Value-axis encoding (Linear or sRGB) used to index the table.
    pub encoding: HueSatEncoding,
}

impl HueSatTable {
    /// Applies the resolved table to an `HSV` sample, honouring the value-axis
    /// `encoding` (F5: an sRGB-encoded DCP map indexes its value axis through the
    /// sRGB curve, dropping this ships wrong color on 3D maps, R3).
    pub fn eval(&self, hsv: [f32; 3]) -> [f32; 3] {
        let delta = interpolate_delta(self.dims, &self.deltas, hsv, self.encoding);
        apply_delta(hsv, delta)
    }

    /// Builds an upload-ready table from a parsed [`HueSatLut`], preserving both
    /// the deltas and the value-axis encoding. The deltas are already in
    /// hue-major order (the DCP parser reorders DNG's value-major layout), so
    /// this is a faithful copy, the encoding is threaded through so
    /// [`HueSatTable::eval`] reproduces [`HueSatLut::eval`] exactly.
    pub fn from_lut(lut: &HueSatLut) -> HueSatTable {
        HueSatTable {
            dims: lut.dims,
            deltas: lut.deltas.clone(),
            encoding: lut.encoding,
        }
    }
}

/// Applies an interpolated `[Δhue°, Δsat×, Δval×]` delta to an HSV sample,
/// wrapping hue, clamping saturation, and floating value at the top.
///
/// **Saturation is clamped, value is not**, and the asymmetry is not an
/// oversight. Saturation is `(max - min) / max`, a ratio that is `[0, 1]` by
/// its own definition, so a `Δsat×` that pushes it past 1 is meaningless and
/// clamping is the only sane reading. Value is the channel maximum of
/// scene-linear working RGB, which has no such ceiling: a highlight that
/// clipped in one sensor channel arrives here above 1 (see
/// `transform::Curve1D`'s docs for where that comes from).
///
/// Clamping value was worse than losing the headroom. `hsv_to_rgb` rebuilds
/// all three channels *from* value, so capping `v` at 1 rescaled the whole
/// pixel by `1 / v`: on `canon-eos-350d.cr2` a blown-sky pixel that should
/// have left here near `[1.95, 1.13, 1.63]` came out `[1.0, 0.62, 0.85]`,
/// which is not the same color one stop down, it is a different, darker,
/// more saturated one. Only the low end is held, at 0, so a negative
/// out-of-gamut channel cannot flip the reconstruction inside out.
///
/// The value *axis lookup* is unaffected and still clamps: `axis_coord` holds
/// at the table's top node for any `v >= 1`, which is right, holding the last
/// authored delta beats extrapolating a LUT off its own end.
fn apply_delta(hsv: [f32; 3], delta: [f32; 3]) -> [f32; 3] {
    let mut h = hsv[0] + delta[0];
    // Wrap hue into [0, 360).
    h %= 360.0;
    if h < 0.0 {
        h += 360.0;
    }
    let s = (hsv[1] * delta[1]).clamp(0.0, 1.0);
    let v = (hsv[2] * delta[2]).max(0.0);
    [h, s, v]
}

/// Trilinear interpolation of the delta cube at `hsv`, hue-wrapped, honouring
/// the value-axis `encoding` and the `val-dims == 1` fast path.
fn interpolate_delta(
    dims: [u32; 3],
    deltas: &[[f32; 3]],
    hsv: [f32; 3],
    encoding: HueSatEncoding,
) -> [f32; 3] {
    let hue_div = dims[0].max(1) as usize;
    let sat_div = dims[1].max(1) as usize;
    let val_div = dims[2].max(1) as usize;

    // Empty/degenerate table ⇒ identity delta.
    if deltas.len() < hue_div * sat_div * val_div || deltas.is_empty() {
        return [0.0, 1.0, 1.0];
    }

    // Hue axis spans the full circle with wrap.
    let hue_coord = (hsv[0].rem_euclid(360.0)) / 360.0 * hue_div as f32;
    let h0 = (hue_coord.floor() as usize) % hue_div;
    let hf = hue_coord - hue_coord.floor();
    let h1 = (h0 + 1) % hue_div;

    // Saturation axis spans [0, 1] over (sat_div - 1) intervals.
    let (s0, s1, sf) = axis_coord(hsv[1], sat_div);

    // Value axis: fast path when there is a single division.
    let (v0, v1, vf) = if val_div == 1 {
        (0usize, 0usize, 0.0f32)
    } else {
        let v_input = match encoding {
            HueSatEncoding::Linear => hsv[2],
            HueSatEncoding::Srgb => srgb_oetf(hsv[2]),
        };
        axis_coord(v_input, val_div)
    };

    let idx =
        |h: usize, s: usize, v: usize| -> [f32; 3] { deltas[(h * sat_div + s) * val_div + v] };

    // Trilinear blend across the eight corners.
    let mut acc = [0.0f32; 3];
    for (hi, hw) in [(h0, 1.0 - hf), (h1, hf)] {
        if hw == 0.0 {
            continue;
        }
        for (si, sw) in [(s0, 1.0 - sf), (s1, sf)] {
            if sw == 0.0 {
                continue;
            }
            for (vi, vw) in [(v0, 1.0 - vf), (v1, vf)] {
                if vw == 0.0 {
                    continue;
                }
                let w = hw * sw * vw;
                let d = idx(hi, si, vi);
                acc[0] += d[0] * w;
                acc[1] += d[1] * w;
                acc[2] += d[2] * w;
            }
        }
    }
    acc
}

/// Maps a `[0, 1]` coordinate onto `div` nodes spanning `(div - 1)` intervals,
/// returning the bracketing node indices and the fractional blend.
fn axis_coord(x: f32, div: usize) -> (usize, usize, f32) {
    if div <= 1 {
        return (0, 0, 0.0);
    }
    let c = (x.clamp(0.0, 1.0)) * (div - 1) as f32;
    let i0 = (c.floor() as usize).min(div - 2);
    let f = c - i0 as f32;
    (i0, i0 + 1, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: [f32; 3], b: [f32; 3], tol: f32) {
        for k in 0..3 {
            assert!((a[k] - b[k]).abs() < tol, "{a:?} vs {b:?} @ {k}");
        }
    }

    #[test]
    fn identity_table_is_a_no_op() {
        let lut = HueSatLut {
            dims: [4, 3, 1],
            deltas: vec![[0.0, 1.0, 1.0]; 4 * 3],
            encoding: HueSatEncoding::Linear,
        };
        approx(lut.eval([123.0, 0.4, 0.6]), [123.0, 0.4, 0.6], 1e-5);
    }

    #[test]
    fn hand_computed_sat_axis_hue_shift() {
        // 1 hue × 2 sat × 1 val: sat 0 shifts +10°, sat 1 shifts +30°.
        let lut = HueSatLut {
            dims: [1, 2, 1],
            deltas: vec![[10.0, 1.0, 1.0], [30.0, 1.0, 1.0]],
            encoding: HueSatEncoding::Linear,
        };
        // At s = 0.5 the shift interpolates to +20°.
        approx(lut.eval([100.0, 0.5, 0.5]), [120.0, 0.5, 0.5], 1e-4);
        // At the nodes it is exact.
        approx(lut.eval([100.0, 0.0, 0.5]), [110.0, 0.0, 0.5], 1e-4);
        approx(lut.eval([100.0, 1.0, 0.5]), [130.0, 1.0, 0.5], 1e-4);
    }

    #[test]
    fn value_axis_scales_and_fast_path() {
        // 1×1×2: value node0 keeps value, node1 halves it.
        let lut = HueSatLut {
            dims: [1, 1, 2],
            deltas: vec![[0.0, 1.0, 1.0], [0.0, 1.0, 0.5]],
            encoding: HueSatEncoding::Linear,
        };
        approx(lut.eval([0.0, 0.0, 1.0]), [0.0, 0.0, 0.5], 1e-4);
        // Midpoint value scale = 0.75 ⇒ 0.5 * 0.75 = 0.375.
        approx(lut.eval([0.0, 0.0, 0.5]), [0.0, 0.0, 0.375], 1e-4);

        // val-dims == 1 fast path ignores the value axis entirely.
        let fast = HueSatLut {
            dims: [1, 1, 1],
            deltas: vec![[0.0, 1.0, 2.0]],
            encoding: HueSatEncoding::Linear,
        };
        approx(fast.eval([0.0, 0.0, 0.25]), [0.0, 0.0, 0.5], 1e-4);
    }

    #[test]
    fn hue_wrap_has_no_seam() {
        // Sat scale varies with hue; the node at 0° = 360° must be shared so
        // eval is continuous across the seam.
        let lut = HueSatLut {
            dims: [4, 1, 1],
            deltas: vec![
                [0.0, 2.0, 1.0], // 0°
                [0.0, 1.0, 1.0], // 90°
                [0.0, 1.0, 1.0], // 180°
                [0.0, 1.0, 1.0], // 270°
            ],
            encoding: HueSatEncoding::Linear,
        };
        let below = lut.eval([359.99, 0.4, 0.5]);
        let above = lut.eval([0.01, 0.4, 0.5]);
        assert!(
            (below[1] - above[1]).abs() < 0.01,
            "seam discontinuity: {below:?} vs {above:?}"
        );
    }

    #[test]
    fn resolved_table_matches_lut_under_srgb_encoding() {
        // F5: the resolved HueSatTable must reproduce HueSatLut::eval exactly,
        // including the sRGB value-axis indexing on a 3D (val-div > 1) table.
        let lut = HueSatLut {
            dims: [2, 2, 3],
            deltas: vec![
                [5.0, 1.1, 0.9],
                [7.0, 1.0, 1.0],
                [3.0, 0.95, 1.05],
                [1.0, 1.2, 0.8],
                [9.0, 1.05, 0.97],
                [2.0, 1.0, 1.0],
                [4.0, 0.9, 1.1],
                [6.0, 1.15, 0.85],
                [8.0, 1.0, 1.0],
                [0.0, 1.0, 1.0],
                [5.5, 1.02, 0.98],
                [3.3, 1.0, 1.0],
            ],
            encoding: HueSatEncoding::Srgb,
        };
        let table = HueSatTable::from_lut(&lut);
        assert_eq!(table.encoding, HueSatEncoding::Srgb);
        for hsv in [
            [30.0, 0.3, 0.2],
            [200.0, 0.7, 0.5],
            [330.0, 0.5, 0.85],
            [95.0, 0.9, 0.05],
        ] {
            approx(table.eval(hsv), lut.eval(hsv), 1e-6);
        }
    }

    #[test]
    fn srgb_encoding_biases_the_value_axis() {
        // With a value-dependent scale, the sRGB encoding remaps where a given
        // linear value lands on the axis, so the two encodings differ.
        let deltas = vec![[0.0, 1.0, 1.0], [0.0, 1.0, 0.5]];
        let lin = HueSatLut {
            dims: [1, 1, 2],
            deltas: deltas.clone(),
            encoding: HueSatEncoding::Linear,
        };
        let srgb = HueSatLut {
            dims: [1, 1, 2],
            deltas,
            encoding: HueSatEncoding::Srgb,
        };
        let v = 0.25;
        let a = lin.eval([0.0, 0.0, v])[2];
        let b = srgb.eval([0.0, 0.0, v])[2];
        assert!((a - b).abs() > 1e-3, "encodings should differ: {a} vs {b}");
    }

    /// Value must pass through above 1 rather than being capped there.
    ///
    /// `value` is the channel max of scene-linear RGB, and `hsv_to_rgb`
    /// rebuilds every channel from it, so capping it at 1 does not clip the
    /// pixel, it rescales the whole pixel by `1 / value`. That is how a raw
    /// file's highlight latitude used to die on the way into the graph.
    #[test]
    fn value_above_one_survives_the_shaping_instead_of_rescaling_the_pixel() {
        let identity = HueSatLut {
            dims: [1, 1, 1],
            deltas: vec![[0.0, 1.0, 1.0]],
            encoding: HueSatEncoding::Linear,
        };
        for v in [1.0f32, 1.25, 2.28, 8.0] {
            let out = identity.eval([200.0, 0.4, v]);
            assert!(
                (out[2] - v).abs() < 1e-5,
                "an identity table must return value {v} unchanged, got {}",
                out[2]
            );
        }

        // A real `Δval×` still applies, it is only the ceiling that is gone.
        let halve = HueSatLut {
            dims: [1, 1, 1],
            deltas: vec![[0.0, 1.0, 0.5]],
            encoding: HueSatEncoding::Linear,
        };
        approx(halve.eval([0.0, 0.0, 3.0]), [0.0, 0.0, 1.5], 1e-5);
    }

    /// The default look's highlight desaturation guard reaches an above-white
    /// pixel and desaturates it *without* pulling it down to white.
    ///
    /// This is the end-to-end shape of the bug that was here: the guard is
    /// authored at the top value node, so every recoverable highlight goes
    /// through it, and it used to take the headroom with it.
    #[test]
    fn the_highlight_desaturation_guard_keeps_the_headroom_it_shapes() {
        let look = crate::look::author_lightbox_color_v1();
        let table = HueSatTable::from_lut(look.hue_sat.as_ref().expect("v1 ships shaping"));
        // A blown-sky pixel of the kind canon-eos-350d.cr2 produces: red well
        // above white because white balance scaled it, green near white.
        let hsv_in = [30.0f32, 0.38, 1.95];
        let out = table.eval(hsv_in);
        assert!(
            out[2] > 1.9,
            "the guard must not cap value: {} came back from {}",
            out[2],
            hsv_in[2]
        );
        assert!(
            out[1] < hsv_in[1],
            "the guard is supposed to desaturate: {} vs {}",
            out[1],
            hsv_in[1]
        );
    }

    /// Saturation is a ratio and stays clamped: the asymmetry with value is
    /// deliberate, so pin it.
    #[test]
    fn saturation_is_still_clamped_to_one() {
        let boost = HueSatLut {
            dims: [1, 1, 1],
            deltas: vec![[0.0, 4.0, 1.0]],
            encoding: HueSatEncoding::Linear,
        };
        assert!((boost.eval([0.0, 0.9, 1.5])[1] - 1.0).abs() < 1e-6);
    }
}
