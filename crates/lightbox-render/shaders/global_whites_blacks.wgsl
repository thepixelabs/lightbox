// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_whites_blacks.wgsl, `global.whites_blacks` (E10 task A8).
// Companion-encoded endpoint remap (spec §4.2 "whites/blacks =
// companion-encoded endpoint remap"): each of R/G/B is encoded via the sRGB
// OETF, linearly remapped from `[bp, wp]` to `[0, 1]` (clamped), then decoded
// back through the sRGB EOTF. Alpha passes through unchanged. This is a
// clamped-linear map in the encoded domain, so it is monotone non-decreasing
// by construction, no tone reversal for any `(bp, wp)`.
//
// `bp`/`wp` (the black/white input levels) are precomputed on the CPU side
// from the `whites`/`blacks` sliders (single source of truth shared with the
// CPU reference path, `nodes::global::whites_blacks::wb_bp_wp`) and handed in
// via the params UBO, so both backends apply the identical remap.
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2).

struct Params {
    bp: f32,
    wp: f32,
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

fn endpoint_remap(e: f32, bp: f32, wp: f32) -> f32 {
    let denom = max(wp - bp, 1e-6);
    return clamp((e - bp) / denom, 0.0, 1.0);
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
    let rr = endpoint_remap(er, params.bp, params.wp);
    let rg = endpoint_remap(eg, params.bp, params.wp);
    let rb = endpoint_remap(eb, params.bp, params.wp);
    let out = vec3<f32>(srgb_eotf(rr), srgb_eotf(rg), srgb_eotf(rb));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(out, c.a));
}
