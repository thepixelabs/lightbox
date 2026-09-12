// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_sharpen.wgsl, `global.sharpen`. Unsharp masking on companion-encoded
// luma with Lightroom's four Detail-panel controls (amount / radius / detail /
// masking).
//
// WHAT THIS IS, HONESTLY: a Gaussian unsharp mask, plus a tanh soft-clip on
// the high-pass amplitude for the `detail` control, plus a Sobel edge mask for
// the `masking` control. It is NOT Adobe's algorithm (theirs blends a
// deconvolution-flavoured kernel in as `detail` rises, and builds its mask from
// a blurred edge map). Same four knobs, same qualitative response, different
// math. See `src/ng/nodes/global/sharpen.rs`'s module doc for the full
// statement of what is and is not matched.
//
// Two dispatches because the Gaussian is separable: `pass_h` writes the
// horizontal 1D pass of the encoded-luma plane into an `f32` scratch texture
// (`dst_ab.r`), `pass_v` finishes the vertical pass out of that scratch and
// does all the per-pixel work. This is the same two-pass shape (and the same
// bind-group layout) `global_clarity.wgsl` uses, including its `f32` scratch
// precision choice, see that file's header for why.
//
// Both passes recompute the SAME normalized 1D Gaussian weights in the same
// loop order as `sharpen.rs`'s `gaussian_kernel`, so CPU and GPU agree to
// float rounding.
//
// Reapply is the identical local-derivative additive delta
// `global_clarity.wgsl` uses (`delta_linear = delta_encoded *
// d(srgb_eotf)/de`) for the identical reason: `amount == 0` must be an EXACT
// pixel identity even for out-of-gamut scene-linear pixels, which a clamped
// `companion_encode`/`companion_decode` round trip would not give.
//
// Unlike `global_clarity.wgsl`/`global_texture.wgsl` there is deliberately NO
// "no new local extremum" clamp here: overshoot at an edge is what sharpening
// IS, and clamping it away would make `amount` do nothing visible. Halo control
// is the `detail` knee and the `masking` gate instead. The only guard is that
// the boosted value stays inside the encoded `[0, 1]` range (`luma_e` is
// already in `[0, 1]` by construction, so that clamp is an exact no-op at
// `amount == 0` and the identity above still holds).

// Worst-case Gaussian half-window: `ceil(3 * sigma)` at the schema's maximum
// radius (`3.0`, `lightbox-edit/src/leaves.rs` `Sharpen::clamp`). Both passes
// always walk the full window and normalize by the weight sum, so a smaller
// radius costs the same and gives the same answer as a truncated window would.
const MAX_BLUR_RADIUS: i32 = 9;
// Encoded-luma amplitude at which the `detail = 0` soft clip is at roughly
// 76% of its input (`tanh(1) = 0.7616`), i.e. where halo suppression starts to
// bite. Fine texture sits well below this; a hard edge's high-pass overshoot
// sits well above it.
const HALO_KNEE: f32 = 0.06;
// Sobel gradient magnitude (encoded luma per pixel) that counts as a fully
// "real" edge at `masking = 100`.
const MASK_FULL_GRADIENT: f32 = 0.20;

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    amount: f32,
    radius: f32,
    detail: f32,
    masking: f32,
    _pad: f32,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var ab_tex: texture_2d<f32>;
@group(1) @binding(0) var dst_ab: texture_storage_2d<rgba32float, write>;
@group(1) @binding(1) var dst_out: texture_storage_2d<rgba16float, write>;
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

fn encoded_luma_of(c: vec4<f32>) -> f32 {
    let l = c.r * params.lw_r + c.g * params.lw_g + c.b * params.lw_b;
    return srgb_oetf(l);
}

fn smoothstep_01(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn blur_sigma() -> f32 {
    return max(params.radius, 1.0e-3);
}

@compute @workgroup_size(16, 16, 1)
fn pass_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let cx = i32(gid.x);
    let cy = i32(gid.y);
    let sigma = blur_sigma();
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    var sum: f32 = 0.0;
    var wsum: f32 = 0.0;
    for (var d: i32 = -MAX_BLUR_RADIUS; d <= MAX_BLUR_RADIUS; d = d + 1) {
        let sx = clamp(cx + d, 0, w - 1);
        let wt = exp(-f32(d * d) * inv_two_sigma_sq);
        sum = sum + wt * encoded_luma_of(textureLoad(src_tex, vec2<i32>(sx, cy), 0));
        wsum = wsum + wt;
    }
    textureStore(dst_ab, vec2<i32>(cx, cy), vec4<f32>(sum / wsum, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(16, 16, 1)
fn pass_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);
    let sigma = blur_sigma();
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    // Vertical half of the separable Gaussian, over `pass_h`'s scratch.
    var sum: f32 = 0.0;
    var wsum: f32 = 0.0;
    for (var d: i32 = -MAX_BLUR_RADIUS; d <= MAX_BLUR_RADIUS; d = d + 1) {
        let sy = clamp(cy + d, 0, h - 1);
        let wt = exp(-f32(d * d) * inv_two_sigma_sq);
        sum = sum + wt * textureLoad(ab_tex, vec2<i32>(cx, sy), 0).r;
        wsum = wsum + wt;
    }
    let blur_e = sum / wsum;

    // Edge mask input: the mean of nine Sobel magnitudes over the 3x3
    // neighborhood, which needs a 5x5 luma window. Averaging the nine keeps
    // a single noisy pixel from reading as an edge; the CPU reference
    // (`sharpen.rs`'s `edge_gradient`) walks the identical window in the
    // identical order.
    var lw: array<f32, 25>;
    for (var j: i32 = -2; j <= 2; j = j + 1) {
        let sy = clamp(cy + j, 0, h - 1);
        for (var i: i32 = -2; i <= 2; i = i + 1) {
            let sx = clamp(cx + i, 0, w - 1);
            lw[(j + 2) * 5 + (i + 2)] = encoded_luma_of(textureLoad(src_tex, vec2<i32>(sx, sy), 0));
        }
    }
    var gsum: f32 = 0.0;
    for (var j: i32 = -1; j <= 1; j = j + 1) {
        for (var i: i32 = -1; i <= 1; i = i + 1) {
            let b = (j + 2) * 5 + (i + 2);
            let gx = (lw[b - 6] + 2.0 * lw[b - 1] + lw[b + 4])
                - (lw[b - 4] + 2.0 * lw[b + 1] + lw[b + 6]);
            let gy = (lw[b - 6] + 2.0 * lw[b - 5] + lw[b - 4])
                - (lw[b + 4] + 2.0 * lw[b + 5] + lw[b + 6]);
            gsum = gsum + sqrt(gx * gx + gy * gy) / 8.0;
        }
    }
    let edge_grad = gsum / 9.0;

    let c = textureLoad(src_tex, vec2<i32>(cx, cy), 0);
    let luma_e = encoded_luma_of(c);

    // ── the four controls ────────────────────────────────────────────────
    let high = luma_e - blur_e;
    // `detail`: soft-clip the high-pass amplitude, then blend back toward the
    // raw high pass as detail rises. `tanh` preserves small (fine-texture)
    // amplitudes to first order and compresses the large ones that become
    // halos at a hard edge.
    let clipped = HALO_KNEE * tanh(high / HALO_KNEE);
    let d01 = clamp(params.detail / 100.0, 0.0, 1.0);
    let high_eff = clipped + (high - clipped) * d01;
    // `masking`: gate by the edge map. At `masking == 0` the mask is exactly
    // 1 everywhere (mix(1, x, 0) == 1 in IEEE-754), so masking costs nothing
    // until it is asked for.
    let m01 = clamp(params.masking / 100.0, 0.0, 1.0);
    let thresh = max(m01 * MASK_FULL_GRADIENT, 1.0e-6);
    let edge = smoothstep_01(0.0, thresh, edge_grad);
    let mask = 1.0 + (edge - 1.0) * m01;

    let raw_delta_encoded = (params.amount / 100.0) * mask * high_eff;
    let boosted = clamp(luma_e + raw_delta_encoded, 0.0, 1.0);
    let delta_encoded = boosted - luma_e;
    let delta_linear = delta_encoded * srgb_eotf_deriv(luma_e);

    var outc = c.rgb + vec3<f32>(delta_linear, delta_linear, delta_linear);
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst_out, vec2<i32>(cx, cy), vec4<f32>(outc, c.a));
}
