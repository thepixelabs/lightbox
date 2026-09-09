// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_tone_curve.wgsl, `global.tone_curve` (E10 task C3). Applies 4
// CPU-baked 1D LUTs (composite/R/G/B monotone-cubic point curve composed
// with the parametric region-slider delta, spec §4.2 "tone curve =
// companion-encoded") via linear-interpolated sampling, the exact mirror of
// `xform.display`'s `eval_curve` shaper pattern (`display.wgsl`). No curve
// or spline math runs on the GPU at all: `nodes::global::tone_curve` bakes
// the LUTs CPU-side from the SAME `nodes::common::curve1d` functions
// `eval_cpu` uses, so CPU/GPU parity holds by construction, not by
// duplicated formulas.
//
// The composite curve applies to R, G, B independently (LR-compatible: this
// is what produces the classic S-curve contrast+saturation look, see the
// domain note in `nodes::global::tone_curve`'s module docs), THEN the
// per-channel R/G/B curves apply on top of their own channel.
//
// Bind groups (§3.2): input @group(0), write-only output storage @group(1),
// params UBO @group(2), baked LUT storage buffers @group(3).

struct Params {
    n: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;
@group(3) @binding(0) var<storage, read> lut_composite: array<f32>;
@group(3) @binding(1) var<storage, read> lut_r: array<f32>;
@group(3) @binding(2) var<storage, read> lut_g: array<f32>;
@group(3) @binding(3) var<storage, read> lut_b: array<f32>;

fn srgb_oetf(u: f32) -> f32 {
    let c = clamp(u, 0.0, 1.0);
    if c <= 0.0031308 {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn srgb_eotf(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if c <= 0.040449936 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

// Linear-interpolated LUT sample, one function per storage array (WGSL has
// no first-class way to pass a `@group(3)` binding by reference between
// functions here), the exact mirror of `display.wgsl`'s `eval_curve`.
fn sample_composite(t: f32) -> f32 {
    let n = params.n;
    if n < 2u {
        return t;
    }
    let p = clamp(t, 0.0, 1.0) * f32(n - 1u);
    let i0 = min(u32(floor(p)), n - 2u);
    let frac = p - f32(i0);
    return lut_composite[i0] * (1.0 - frac) + lut_composite[i0 + 1u] * frac;
}

fn sample_r(t: f32) -> f32 {
    let n = params.n;
    if n < 2u {
        return t;
    }
    let p = clamp(t, 0.0, 1.0) * f32(n - 1u);
    let i0 = min(u32(floor(p)), n - 2u);
    let frac = p - f32(i0);
    return lut_r[i0] * (1.0 - frac) + lut_r[i0 + 1u] * frac;
}

fn sample_g(t: f32) -> f32 {
    let n = params.n;
    if n < 2u {
        return t;
    }
    let p = clamp(t, 0.0, 1.0) * f32(n - 1u);
    let i0 = min(u32(floor(p)), n - 2u);
    let frac = p - f32(i0);
    return lut_g[i0] * (1.0 - frac) + lut_g[i0 + 1u] * frac;
}

fn sample_b(t: f32) -> f32 {
    let n = params.n;
    if n < 2u {
        return t;
    }
    let p = clamp(t, 0.0, 1.0) * f32(n - 1u);
    let i0 = min(u32(floor(p)), n - 2u);
    let frac = p - f32(i0);
    return lut_b[i0] * (1.0 - frac) + lut_b[i0 + 1u] * frac;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let er = srgb_oetf(c.r);
    let eg = srgb_oetf(c.g);
    let eb = srgb_oetf(c.b);
    let mr = sample_composite(er);
    let mg = sample_composite(eg);
    let mb = sample_composite(eb);
    let fr = sample_r(mr);
    let fg = sample_g(mg);
    let fb = sample_b(mb);
    let out = vec3<f32>(srgb_eotf(fr), srgb_eotf(fg), srgb_eotf(fb));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(out, clamp(c.a, 0.0, 1.0)));
}
