// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_noise_reduction.wgsl, `global.noise_reduction`. Luminance and chroma
// denoise with Lightroom's four Detail-panel noise controls (luma /
// luma_detail / chroma / chroma_detail).
//
// WHAT THIS IS, HONESTLY: two joint bilateral filters over a companion-encoded
// luma/chroma decomposition, one narrow one on luma and one wide one on the
// two chroma-difference channels, each blended back by its own strength
// slider. It is NOT Adobe's algorithm (theirs is a multi-scale, wavelet-style
// denoiser and the AI variant is a network). A bilateral filter is the honest
// baseline named in this node's brief; it is edge-aware, so it is not the
// "naive box blur that destroys detail" the brief rules out, but it does not
// separate noise from fine texture as well as a multi-scale method does. See
// `src/ng/nodes/global/noise_reduction.rs`'s module doc for the full statement.
//
// ONE dispatch. The luma window is a strict sub-window of the chroma window
// (same center, `LUMA_RADIUS < CHROMA_RADIUS`), so both filters accumulate in
// a single window loop, the same trick `global_texture.wgsl` uses for its two
// box radii.
//
// Chroma first, in effort terms: chroma noise is the coloured blotching, it is
// low-frequency, and a wide edge-aware average removes it convincingly at a
// cost luma denoising cannot match, which is why `CHROMA_RADIUS` is the larger
// of the two.
//
// Reapply is the local-derivative additive delta `global_clarity.wgsl`
// introduced, extended to three independent per-channel deltas (this node
// moves chroma as well as luma, so a single shared scalar delta will not do):
// the luma/chroma deltas are converted to per-channel ENCODED deltas whose
// luma-weighted sum is exactly the luma delta, then each is scaled by
// `d(srgb_eotf)/de` at that channel's own encoded value. Every delta is
// exactly `0` when its strength slider is `0`, and `0 * finite == 0` in
// IEEE-754, so a neutral node is an exact pixel identity even for
// out-of-gamut scene-linear pixels.

// Spatial half-windows, pixels. Chroma gets the wider one (see the header).
const LUMA_RADIUS: i32 = 3;
const CHROMA_RADIUS: i32 = 5;
// Spatial Gaussian sigmas, roughly half of each radius so the window's corner
// taps still carry meaningful weight.
const LUMA_SPATIAL_SIGMA: f32 = 1.6;
const CHROMA_SPATIAL_SIGMA: f32 = 2.6;
// Range (photometric) sigmas, in companion-encoded units. The `*_detail`
// sliders interpolate MAX (detail 0, smooth hard, edges included) to MIN
// (detail 100, only near-identical neighbours average, detail survives).
const LUMA_RANGE_SIGMA_MAX: f32 = 0.060;
const LUMA_RANGE_SIGMA_MIN: f32 = 0.006;
const CHROMA_RANGE_SIGMA_MAX: f32 = 0.120;
const CHROMA_RANGE_SIGMA_MIN: f32 = 0.010;

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    luma: f32,
    luma_detail: f32,
    chroma: f32,
    chroma_detail: f32,
    _pad: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

fn srgb_oetf(u: f32) -> f32 {
    let c = clamp(u, 0.0, 1.0);
    if c <= 0.0031308 {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn srgb_eotf_deriv(e: f32) -> f32 {
    let c = clamp(e, 0.0, 1.0);
    if c <= 0.040449936 {
        return 1.0 / 12.92;
    }
    return (2.4 / 1.055) * pow((c + 0.055) / 1.055, 1.4);
}

// Per-channel companion encode, then the (Y, Cb, Cr)-shaped opponent triple
// this node filters in: `y = dot(e, luma_weights)`, `cb = e.b - y`,
// `cr = e.r - y`.
fn ycc_of(c: vec4<f32>) -> vec3<f32> {
    let e = vec3<f32>(srgb_oetf(c.r), srgb_oetf(c.g), srgb_oetf(c.b));
    let y = e.r * params.lw_r + e.g * params.lw_g + e.b * params.lw_b;
    return vec3<f32>(y, e.b - y, e.r - y);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims_dst = textureDimensions(dst);
    if gid.x >= dims_dst.x || gid.y >= dims_dst.y {
        return;
    }
    let dims = textureDimensions(src);
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);

    let c = textureLoad(src, vec2<i32>(cx, cy), 0);
    let center = ycc_of(c);

    let sl = mix(
        LUMA_RANGE_SIGMA_MAX,
        LUMA_RANGE_SIGMA_MIN,
        clamp(params.luma_detail / 100.0, 0.0, 1.0),
    );
    let sc = mix(
        CHROMA_RANGE_SIGMA_MAX,
        CHROMA_RANGE_SIGMA_MIN,
        clamp(params.chroma_detail / 100.0, 0.0, 1.0),
    );
    let inv_l_spatial = 1.0 / (2.0 * LUMA_SPATIAL_SIGMA * LUMA_SPATIAL_SIGMA);
    let inv_c_spatial = 1.0 / (2.0 * CHROMA_SPATIAL_SIGMA * CHROMA_SPATIAL_SIGMA);
    let inv_l_range = 1.0 / (2.0 * sl * sl);
    let inv_c_range = 1.0 / (2.0 * sc * sc);

    var acc_y: f32 = 0.0;
    var sum_l: f32 = 0.0;
    var acc_cb: f32 = 0.0;
    var acc_cr: f32 = 0.0;
    var sum_c: f32 = 0.0;
    for (var dy: i32 = -CHROMA_RADIUS; dy <= CHROMA_RADIUS; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -CHROMA_RADIUS; dx <= CHROMA_RADIUS; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let tap = ycc_of(textureLoad(src, vec2<i32>(sx, sy), 0));
            let d2 = f32(dx * dx + dy * dy);

            let dcb = tap.g - center.g;
            let dcr = tap.b - center.b;
            let wc = exp(-d2 * inv_c_spatial) * exp(-(dcb * dcb + dcr * dcr) * inv_c_range);
            acc_cb = acc_cb + wc * tap.g;
            acc_cr = acc_cr + wc * tap.b;
            sum_c = sum_c + wc;

            if abs(dx) <= LUMA_RADIUS && abs(dy) <= LUMA_RADIUS {
                let dl = tap.r - center.r;
                let wl = exp(-d2 * inv_l_spatial) * exp(-dl * dl * inv_l_range);
                acc_y = acc_y + wl * tap.r;
                sum_l = sum_l + wl;
            }
        }
    }
    // The center tap's own weight is `exp(0) * exp(0) == 1` exactly, so both
    // sums are >= 1 and neither division can be by zero.
    let y_f = acc_y / sum_l;
    let cb_f = acc_cb / sum_c;
    let cr_f = acc_cr / sum_c;

    let d_y = (y_f - center.r) * clamp(params.luma / 100.0, 0.0, 1.0);
    let d_cb = (cb_f - center.g) * clamp(params.chroma / 100.0, 0.0, 1.0);
    let d_cr = (cr_f - center.b) * clamp(params.chroma / 100.0, 0.0, 1.0);

    // Per-channel encoded deltas. `de_g` is solved so that
    // `dot(de, luma_weights) == d_y` exactly (the weights sum to 1), which is
    // what keeps the chroma move from dragging luminance with it.
    let de_r = d_y + d_cr;
    let de_b = d_y + d_cb;
    let de_g = d_y - (params.lw_r * d_cr + params.lw_b * d_cb) / params.lw_g;

    let e = vec3<f32>(srgb_oetf(c.r), srgb_oetf(c.g), srgb_oetf(c.b));
    let delta_linear = vec3<f32>(
        de_r * srgb_eotf_deriv(e.r),
        de_g * srgb_eotf_deriv(e.g),
        de_b * srgb_eotf_deriv(e.b),
    );

    var outc = c.rgb + delta_linear;
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst, vec2<i32>(cx, cy), vec4<f32>(outc, c.a));
}
