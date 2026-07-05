// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0
//
// display.transform — GPU implementation of the algorithm specified in
// display_transform.md (E01 spec §3.4/§5 T23). The CPU path in
// display_transform.rs implements the SAME written spec with identical
// filter coordinates; CPU/GPU ΔE2000 parity is a PR gate (§4.4).
//
// Input:  texture_2d<f32> over Rgba8Unorm — texel values are sRGB-ENCODED
//         bytes read as unorm floats (c = byte/255); decode is explicit.
// Output: storage texture rgba8unorm — values written sRGB-encoded.

struct Params {
    orientation: u32, // EXIF 1..=8 (validated on the CPU side)
    out_w: u32,
    out_h: u32,
    in_w: u32,
    in_h: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;

// sRGB EOTF (decode), IEC 61966-2-1 — see display_transform.md.
fn srgb_decode(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

// sRGB OETF (encode).
fn srgb_encode(c: f32) -> f32 {
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// Oriented (upright) integer coordinates -> stored coordinates. The same
// table as display_transform.md / `orient_map` in display_transform.rs.
fn orient_map(x: u32, y: u32) -> vec2<u32> {
    let sw = params.in_w;
    let sh = params.in_h;
    switch params.orientation {
        case 2u: { return vec2(sw - 1u - x, y); }
        case 3u: { return vec2(sw - 1u - x, sh - 1u - y); }
        case 4u: { return vec2(x, sh - 1u - y); }
        case 5u: { return vec2(y, x); }
        case 6u: { return vec2(y, sh - 1u - x); }
        case 7u: { return vec2(sw - 1u - y, sh - 1u - x); }
        case 8u: { return vec2(sw - 1u - y, x); }
        default: { return vec2(x, y); }
    }
}

// One tap: clamp in oriented space, map to stored space, fetch, decode
// RGB to linear light (alpha is coverage — normalized but not decoded).
fn fetch_linear(ou: i32, ov: i32, ow: i32, oh: i32) -> vec4<f32> {
    let cx = u32(clamp(ou, 0, ow - 1));
    let cy = u32(clamp(ov, 0, oh - 1));
    let s = orient_map(cx, cy);
    let t = textureLoad(src, vec2<i32>(s), 0);
    return vec4(srgb_decode(t.r), srgb_decode(t.g), srgb_decode(t.b), t.a);
}

// lerp(a, b, t) = a*(1-t) + b*t — spelled out (not the `mix` builtin) so the
// association matches the CPU path exactly (display_transform.md step 4).
fn lerp4(a: vec4<f32>, b: vec4<f32>, t: f32) -> vec4<f32> {
    return a * (1.0 - t) + b * t;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.out_w || gid.y >= params.out_h {
        return;
    }

    // Oriented dimensions: the transposing family (5..8) swaps.
    var ow = i32(params.in_w);
    var oh = i32(params.in_h);
    if params.orientation >= 5u {
        let t = ow;
        ow = oh;
        oh = t;
    }

    // Filter coordinates, pixel-center convention (display_transform.md
    // step 1 — the exact association matters for parity).
    let u = (f32(gid.x) + 0.5) * f32(ow) / f32(params.out_w) - 0.5;
    let v = (f32(gid.y) + 0.5) * f32(oh) / f32(params.out_h) - 0.5;
    let u0 = floor(u);
    let v0 = floor(v);
    let fu = u - u0;
    let fv = v - v0;
    let iu = i32(u0);
    let iv = i32(v0);

    let c00 = fetch_linear(iu, iv, ow, oh);
    let c10 = fetch_linear(iu + 1, iv, ow, oh);
    let c01 = fetch_linear(iu, iv + 1, ow, oh);
    let c11 = fetch_linear(iu + 1, iv + 1, ow, oh);
    let lin = lerp4(lerp4(c00, c10, fu), lerp4(c01, c11, fu), fv);

    let out = vec4(
        clamp(srgb_encode(lin.r), 0.0, 1.0),
        clamp(srgb_encode(lin.g), 0.0, 1.0),
        clamp(srgb_encode(lin.b), 0.0, 1.0),
        clamp(lin.a, 0.0, 1.0),
    );
    textureStore(dst, vec2<i32>(gid.xy), out);
}
