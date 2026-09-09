// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_clarity.wgsl, `global.clarity` (E10 task D3). Guided-filter local
// contrast on companion-encoded luma, midtone-weighted (spec §4.2 "Clarity /
// texture | guided-filter (clarity) ... on companion-encoded luma, detail
// recombined"). Clean-room from He & Sun, *Fast Guided Filter* (2015),
// self-guided case, the identical two-pass shape
// `global_tone_recovery.wgsl` already uses (that file's header explains the
// `f32` scratch precision choice; this kernel makes the same choice for the
// same numerical-stability reason).
//
// # Reapply: a local-derivative additive delta, not a clamped round-trip
//
// `clarity == 0` must be an EXACT pixel identity for every input, including
// out-of-gamut (negative or > 1) scene-linear values, the same "is_identity
// ⇒ exact bit identity" contract every other E10 node upholds. Going through
// a full `companion_encode → adjust → companion_decode` round trip on the
// full RGB triple would NOT be exact for out-of-gamut pixels (the encode
// step's `[0,1]` clamp is lossy), so instead: the local-contrast delta is
// computed as a SCALAR in the encoded-luma domain (`delta_encoded`, exactly
// `0` at `clarity == 0`), then converted to a linear-domain delta via the
// FIRST-DERIVATIVE of the sRGB EOTF at the (clamped) operating point
// `delta_linear = delta_encoded * d(srgb_eotf)/de`, and added identically to
// all three channels. `0 * finite == 0` in IEEE-754 regardless of the
// derivative's value, so the identity holds unconditionally, not merely for
// in-gamut pixels; away from `clarity == 0` this is a first-order Taylor
// approximation of the "adjust in gamma space, decode back" operation, which
// is calibration latitude (the same "implementer picks the mapping" freedom
// `contrast`/`whites_blacks` already take for their own constants).
//
// Bind groups mirror `global_tone_recovery.wgsl` exactly: `src_tex` +
// `ab_tex` @group(0), `dst_ab` (f32 scratch) + `dst_out` (real output)
// @group(1), one params UBO @group(2).

const RADIUS: i32 = 12;
const EPS: f32 = 4.0e-3;
const LOW_EDGE: f32 = 0.12;
const HIGH_EDGE: f32 = 0.88;
const CLARITY_RANGE: f32 = 1.0;
const GAIN_MIN: f32 = 0.0;
const GAIN_MAX: f32 = 2.0;

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    clarity: f32,
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

fn midtone_weight(e: f32) -> f32 {
    let lo = smoothstep_01(0.0, LOW_EDGE, e);
    let hi = 1.0 - smoothstep_01(HIGH_EDGE, 1.0, e);
    return clamp(lo * hi, 0.0, 1.0);
}

@compute @workgroup_size(16, 16, 1)
fn pass_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);

    var sum_l: f32 = 0.0;
    var sum_l2: f32 = 0.0;
    for (var dy: i32 = -RADIUS; dy <= RADIUS; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -RADIUS; dx <= RADIUS; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let v = encoded_luma_of(textureLoad(src_tex, vec2<i32>(sx, sy), 0));
            sum_l = sum_l + v;
            sum_l2 = sum_l2 + v * v;
        }
    }
    let n = f32((2 * RADIUS + 1) * (2 * RADIUS + 1));
    let mean_l = sum_l / n;
    let mean_l2 = sum_l2 / n;
    let var_l = max(mean_l2 - mean_l * mean_l, 0.0);
    let a = var_l / (var_l + EPS);
    let b = mean_l * (1.0 - a);
    textureStore(dst_ab, vec2<i32>(cx, cy), vec4<f32>(a, b, 0.0, 0.0));
}

@compute @workgroup_size(16, 16, 1)
fn pass_c(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);

    var sum_a: f32 = 0.0;
    var sum_b: f32 = 0.0;
    // The halo guard (task D3 AC, see `apply_clarity`'s Rust doc comment
    // for the full rationale): min/max of the encoded luma over the SAME
    // window, accumulated in this same loop (one extra `src_tex` load per
    // tap, avoiding a third dispatch).
    var local_min: f32 = 1.0e30;
    var local_max: f32 = -1.0e30;
    for (var dy: i32 = -RADIUS; dy <= RADIUS; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -RADIUS; dx <= RADIUS; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let ab = textureLoad(ab_tex, vec2<i32>(sx, sy), 0);
            sum_a = sum_a + ab.r;
            sum_b = sum_b + ab.g;
            let tap_luma = encoded_luma_of(textureLoad(src_tex, vec2<i32>(sx, sy), 0));
            local_min = min(local_min, tap_luma);
            local_max = max(local_max, tap_luma);
        }
    }
    let n = f32((2 * RADIUS + 1) * (2 * RADIUS + 1));
    let mean_a = sum_a / n;
    let mean_b = sum_b / n;

    let c = textureLoad(src_tex, vec2<i32>(cx, cy), 0);
    let luma_e = encoded_luma_of(c);
    let base_e = mean_a * luma_e + mean_b;
    let detail = luma_e - base_e;

    let weight = midtone_weight(luma_e);
    let gain = clamp(1.0 + (params.clarity / 100.0) * CLARITY_RANGE * weight, GAIN_MIN, GAIN_MAX);
    // See the Rust-side `apply_clarity`'s comment: `boosted` MUST be
    // `luma_e + raw_delta_encoded` (not `base_e + detail * gain`) so it is
    // bit-exact to `luma_e` at `gain == 1.0`.
    let raw_delta_encoded = detail * (gain - 1.0);
    let boosted = luma_e + raw_delta_encoded;
    let clamped = clamp(boosted, local_min, local_max);
    let delta_encoded = clamped - luma_e;
    let delta_linear = delta_encoded * srgb_eotf_deriv(luma_e);

    var outc = c.rgb + vec3<f32>(delta_linear, delta_linear, delta_linear);
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst_out, vec2<i32>(cx, cy), vec4<f32>(outc, c.a));
}
