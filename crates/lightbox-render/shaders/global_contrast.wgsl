// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_contrast.wgsl, `global.contrast` (E10 task A7). Companion-encoded
// sigmoid-family pivot curve (spec §4.2 "contrast = companion-encoded,
// sigmoid pivoted at encoded 18% gray"): each of R/G/B is encoded via the
// sRGB OETF, remapped by a two-branch power curve pinned exactly at the
// pivot and at both endpoints (0 and 1 are fixed points for any `gamma`),
// then decoded back through the sRGB EOTF. Alpha passes through unchanged.
//
// `gamma` and `pivot` are precomputed on the CPU side (single source of
// truth shared with the CPU reference path,
// `nodes::global::contrast::{contrast_gamma, contrast_pivot}`) and handed in
// via the params UBO, so both backends apply the identical curve.
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2).

struct Params {
    gamma: f32,
    pivot: f32,
    _pad0: f32,
    _pad1: f32,
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

fn srgb_eotf(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if c <= 0.040449936 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

// Pivot-anchored two-branch power curve: continuous and pinned at (0,0),
// (pivot,pivot), (1,1) for any `gamma` > 0. `gamma` > 1 steepens the pivot
// (more contrast); `gamma` < 1 flattens it (less contrast).
fn curve(e: f32, pivot: f32, gamma: f32) -> f32 {
    if e <= pivot {
        return pivot * pow(e / pivot, gamma);
    }
    return 1.0 - (1.0 - pivot) * pow((1.0 - e) / (1.0 - pivot), gamma);
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
    let cr = curve(er, params.pivot, params.gamma);
    let cg = curve(eg, params.pivot, params.gamma);
    let cb = curve(eb, params.pivot, params.gamma);
    let out = vec3<f32>(srgb_eotf(cr), srgb_eotf(cg), srgb_eotf(cb));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(out, c.a));
}
