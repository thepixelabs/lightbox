// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// display.wgsl, xform.display working→display transform (E05 §1 engine-owned
// node; A-gpu). The engine holds ZERO color science: the shaper curve and 65³
// LUT are BAKED BY `lightbox-color` (`build_display_transform`) and uploaded
// here; this kernel only APPLIES them, `out = trilinear(lut, shaper(rgb))`
// which is the exact GPU form `lightbox-color::display` documents as the parity
// anchor its CPU `DisplayTransform::apply` matches (E02 §3.5).
//
// Bind groups (§3.2): input working tile @group(0), write-only display output
// storage @group(1), params UBO @group(2), baked LUT/shaper aux @group(3).

struct Params {
    shaper_n: u32,
    lut_n: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba8unorm, write>;
@group(2) @binding(0) var<uniform> params: Params;
@group(3) @binding(0) var<storage, read> shaper: array<f32>;
@group(3) @binding(1) var<storage, read> lut: array<vec4<f32>>;

// 1-D shaper: linear interpolation over `shaper_n` samples in [0,1], the exact
// mirror of `lightbox_color::display::eval_curve`.
fn eval_curve(t: f32) -> f32 {
    let n = params.shaper_n;
    if n == 0u {
        return t;
    }
    if n == 1u {
        return shaper[0];
    }
    let p = clamp(t, 0.0, 1.0) * f32(n - 1u);
    let i0 = min(u32(floor(p)), n - 2u);
    let frac = p - f32(i0);
    return shaper[i0] * (1.0 - frac) + shaper[i0 + 1u] * frac;
}

fn lut_at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let n = params.lut_n;
    return lut[r + n * (g + n * b)].xyz;
}

// Trilinear cube-LUT sample, the exact mirror of
// `lightbox_color::display::sample_lut` (r fastest, i0 clamped to n-2).
fn sample_lut(c: vec3<f32>) -> vec3<f32> {
    let n = params.lut_n;
    if n < 2u {
        return c;
    }
    let last = f32(n - 1u);
    let vr = clamp(c.r, 0.0, 1.0) * last;
    let vg = clamp(c.g, 0.0, 1.0) * last;
    let vb = clamp(c.b, 0.0, 1.0) * last;
    let r0 = min(u32(floor(vr)), n - 2u);
    let g0 = min(u32(floor(vg)), n - 2u);
    let b0 = min(u32(floor(vb)), n - 2u);
    let rf = vr - f32(r0);
    let gf = vg - f32(g0);
    let bf = vb - f32(b0);

    let c000 = lut_at(r0, g0, b0);
    let c100 = lut_at(r0 + 1u, g0, b0);
    let c010 = lut_at(r0, g0 + 1u, b0);
    let c110 = lut_at(r0 + 1u, g0 + 1u, b0);
    let c001 = lut_at(r0, g0, b0 + 1u);
    let c101 = lut_at(r0 + 1u, g0, b0 + 1u);
    let c011 = lut_at(r0, g0 + 1u, b0 + 1u);
    let c111 = lut_at(r0 + 1u, g0 + 1u, b0 + 1u);

    let c00 = c000 * (1.0 - rf) + c100 * rf;
    let c10 = c010 * (1.0 - rf) + c110 * rf;
    let c01 = c001 * (1.0 - rf) + c101 * rf;
    let c11 = c011 * (1.0 - rf) + c111 * rf;
    let c0 = c00 * (1.0 - gf) + c10 * gf;
    let c1 = c01 * (1.0 - gf) + c11 * gf;
    return c0 * (1.0 - bf) + c1 * bf;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let px = textureLoad(src, vec2<i32>(gid.xy), 0);
    let shaped = vec3<f32>(eval_curve(px.r), eval_curve(px.g), eval_curve(px.b));
    let disp = sample_lut(shaped);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(clamp(disp, vec3<f32>(0.0), vec3<f32>(1.0)), clamp(px.a, 0.0, 1.0)));
}
