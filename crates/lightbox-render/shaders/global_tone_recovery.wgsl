// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_tone_recovery.wgsl, `global.tone_recovery`, the E10 §6 M1-slice
// **PROVISIONAL** `ToneRecoveryNode`. Fast guided-filter base/detail
// highlight/shadow recovery, clean-room from He & Sun, "Fast Guided Filter"
// (2015) / He, Sun, Tang, "Guided Image Filtering" (2010), self-guided
// (I = p = scene-linear luma); spec §4.5 candidate 1. No GPL source consulted.
//
// Two passes over one shared `Params` UBO (mirrors
// `nodes::global::tone_recovery`'s CPU reference exactly, parity verified by
// `tests/e10_tone_recovery.rs`'s B12 suite):
//
//   pass_a: brute-force box-filter (radius `RADIUS`, clamp-to-edge taps) the
//           self-guided local regression coefficients `(a, b)` from the He &
//           Sun linear model `q_i = a_k*I_i + b_k` (their eq. 4-6, self-guided
//           so `var_I == cov_Ip`), written to a scratch `rgba32float` texture
//           (`.rg` used), **not** `rgba16float`: `a = var/(var+GUIDE_EPS)`
//           is numerically stiff near `var ≈ GUIDE_EPS` (its derivative peaks
//           at `1/(4·GUIDE_EPS)`), so rounding `a`/`b` to `f16` between the
//           two passes was measurably widening CPU/GPU parity beyond the
//           §4.4 ΔE2000 ≤ 1.0 bound on smooth, low-variance corpus scenes
//           (e.g. `HighKey`), an extra precision-loss step the CPU
//           reference (`f32` throughout, per spec §4.2) never takes. `f32`
//           scratch removes it; the two backends now agree to plain
//           summation-order rounding, well inside tolerance (task B12).
//   pass_c: box-filters `(a, b)` back to `(mean_a, mean_b)`, recombines the
//           guided-filter base `q = mean_a * L + mean_b` (their eq. 8),
//           applies the asymmetric highlight/shadow range compression to the
//           base only (spec §4.5's "asymmetric range compression of the
//           base"), re-adds the `L - q` detail layer with a gain clamp, and
//           reapplies the result to RGB via a luma-ratio with a chroma-guard
//           clamp (spec §4.5's "reapply to RGB via luma-ratio with a chroma
//           guard").
//
// DEFERRED to M2 (docs/plan/epics/E10-deviations.md): fast local-Laplacian
// (halo-free by construction, spec §4.5 candidate 2) is the target quality
// bar; this guided-filter path can show residual gradient-reversal halos at
// strong, high-contrast edges (exactly the failure mode
// `lightbox_render_testkit::halo`'s B2 metric is built to catch), the M1
// provisional node accepts that risk for O(N) cost and zero pyramid infra.
// `RADIUS`/`EPS`/the region-weight and gain-clamp constants below are fixed
// **algorithm** constants (not recipe params), duplicated *literally* in
// `nodes::global::tone_recovery`'s CPU reference rather than shared through
// the params UBO, exactly the established per-node convention already used by
// `global_whites_blacks.wgsl` (its sRGB OETF/EOTF pair) and
// `global_contrast.wgsl` (`contrast_gamma`/`contrast_pivot` computed once on
// the CPU side and only the *result* handed through the UBO), CPU/GPU parity
// tests catch any drift between the two copies.

const RADIUS: i32 = 8;
// Kept in exact sync with the Rust-side `GUIDE_EPS` in
// `nodes/global/tone_recovery.rs` (see that constant's doc comment for the
// numerical-stability derivation, not just an algorithm-tuning rationale).
const GUIDE_EPS: f32 = 4.0e-3;
const MID_GRAY: f32 = 0.18;
const WEIGHT_KNEE_STOPS: f32 = 2.0;
// MUST satisfy `MAX_STOPS * 0.75 < 1.0` for `compression_gain`'s base
// recompression to stay monotone (see the Rust-side `MAX_STOPS` doc comment
// in `nodes/global/tone_recovery.rs` for the derivation), kept in exact sync
// with that constant; CPU/GPU parity tests catch any drift between the two.
const MAX_STOPS: f32 = 1.0;
const DETAIL_GAIN_MIN: f32 = 0.5;
const DETAIL_GAIN_MAX: f32 = 1.8;
const LUMA_EPS: f32 = 1.0e-4;
const RATIO_MAX: f32 = 8.0;

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    highlights: f32,
    shadows: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

// `ab_tex`/`dst_ab` are unreferenced by `pass_c`/`pass_a` respectively (each
// writes/reads only its own group(1) slot, `dst_ab` (the `f32` scratch) for
// `pass_a`, `dst_out` (the node's real `f16` output tile) for `pass_c`); each
// pipeline's bind-group layout is naga-auto-derived per entry-point
// *reachability* (see `KernelBuilder`'s own docs), so `pass_a`'s @group(0)
// layout has exactly one binding (0) and `pass_c`'s has two (0, 1), and
// likewise for @group(1) (`dst_ab` at binding 0, `dst_out` at binding 1)
// the Rust side builds each pass's bind group to match.
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var ab_tex: texture_2d<f32>;
@group(1) @binding(0) var dst_ab: texture_storage_2d<rgba32float, write>;
@group(1) @binding(1) var dst_out: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

fn luma_of(c: vec4<f32>) -> f32 {
    return c.r * params.lw_r + c.g * params.lw_g + c.b * params.lw_b;
}

fn smoothstep_01(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Stops of `l` from `MID_GRAY` (negative = darker than mid-gray).
fn stops_from_luma(l: f32) -> f32 {
    return log2(max(l, LUMA_EPS) / MID_GRAY);
}

// The B7 param mapping: (highlights, shadows) -> a multiplicative luma gain
// evaluated at the guided-filter base value. `highlights` compresses (pulls
// down) bright quarter-tones; `shadows` lifts dark quarter-tones. The two
// region weights (`hw`/`sw`) are disjoint by construction (`smoothstep_01`
// saturates to exactly 0 past its zero edge), so a positive `highlights` can
// never move a deep-shadow pixel ("highlights never lift blacks") and a
// positive `shadows` can never move a blown-highlight pixel ("shadows never
// pull down whites"), both asymmetry rules hold structurally, not by
// clamping. `highlights == shadows == 0` gives `stops_delta == 0` for every
// `base_luma`, i.e. `gain == 1` everywhere, the exact-identity contract
// `is_identity` elides the whole node on.
fn compression_gain(base_luma: f32) -> f32 {
    let stops = stops_from_luma(base_luma);
    let hw = smoothstep_01(0.0, WEIGHT_KNEE_STOPS, stops);
    let sw = smoothstep_01(0.0, WEIGHT_KNEE_STOPS, -stops);
    let stops_delta = (params.shadows / 100.0) * sw * MAX_STOPS
        - (params.highlights / 100.0) * hw * MAX_STOPS;
    return exp2(stops_delta);
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
            let v = luma_of(textureLoad(src_tex, vec2<i32>(sx, sy), 0));
            sum_l = sum_l + v;
            sum_l2 = sum_l2 + v * v;
        }
    }
    let n = f32((2 * RADIUS + 1) * (2 * RADIUS + 1));
    let mean_l = sum_l / n;
    let mean_l2 = sum_l2 / n;
    let var_l = max(mean_l2 - mean_l * mean_l, 0.0);
    let a = var_l / (var_l + GUIDE_EPS);
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
    for (var dy: i32 = -RADIUS; dy <= RADIUS; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -RADIUS; dx <= RADIUS; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let ab = textureLoad(ab_tex, vec2<i32>(sx, sy), 0);
            sum_a = sum_a + ab.r;
            sum_b = sum_b + ab.g;
        }
    }
    let n = f32((2 * RADIUS + 1) * (2 * RADIUS + 1));
    let mean_a = sum_a / n;
    let mean_b = sum_b / n;

    let c = textureLoad(src_tex, vec2<i32>(cx, cy), 0);
    let luma = luma_of(c);
    let base = mean_a * luma + mean_b;
    let detail = luma - base;

    let gain = compression_gain(base);
    let new_base = base * gain;
    let dgain = clamp(gain, DETAIL_GAIN_MIN, DETAIL_GAIN_MAX);
    let out_luma = new_base + detail * dgain;
    // Chroma guard: reapply via a uniform RGB gain (exact hue/chroma
    // preservation, scaling r,g,b by the same ratio never changes their
    // proportions) clamped so a near-zero-luma pixel's ratio can never blow
    // up (`LUMA_EPS` floor on the divisor, `RATIO_MAX` ceiling on the ratio).
    let ratio = clamp(out_luma / max(luma, LUMA_EPS), 0.0, RATIO_MAX);

    var outc = vec3<f32>(c.r * ratio, c.g * ratio, c.b * ratio);
    // Defensive NaN guard (B11 "no NaN/Inf on fuzzed inputs"): `x != x` is
    // true only for NaN under IEEE-754, so a non-finite result anywhere
    // falls back to the untouched input pixel rather than propagating.
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst_out, vec2<i32>(cx, cy), vec4<f32>(outc, c.a));
}
