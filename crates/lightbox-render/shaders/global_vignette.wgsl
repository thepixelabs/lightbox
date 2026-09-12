// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_vignette.wgsl, `fx.vignette`: the POST-CROP vignette from
// `lightbox_edit::leaves::PostCropVignette` (amount / midpoint / roundness /
// feather / highlights).
//
// Post-crop means the falloff is centred on the CROPPED canvas, not the
// original frame. The node is spliced after the geometry segment for exactly
// that reason (see `nodes::global::build_effects_segment`), and the canvas
// size it normalizes against rides in the params UBO (`canvas_w`/`canvas_h`),
// baked at compile time from `geom.crop`'s own rounded output extent.
//
// The kernel is pointwise: an elliptical falloff `t` in [0,1] derived from
// the pixel's normalized position, a linear gain `1 + (amount/100) * t`, and
// a highlight-protection term that pulls the gain back toward 1 on bright
// pixels. `scale_x`/`scale_y` (the roundness aspect morph), `mid` (the
// midpoint radius) and `feather` (the falloff half-width) are all
// precomputed on the CPU side so the two backends share one source of truth
// for them (`nodes::global::vignette::{vignette_axis_scales,
// vignette_mid_radius, vignette_feather_halfwidth}`); the per-pixel formulas
// below are duplicated literally from that module's `vignette_falloff` /
// `apply_vignette`, the established per-node convention, and the CPU/GPU
// parity test catches any drift.
//
// `off_x`/`off_y` are the output tile's origin in canvas pixels, the same
// `input.roi` origin `geom_crop.wgsl` threads through (`nodes/geometry/
// crop.rs:222`). The GPU backend dispatches whole-canvas today, so they are
// zero there (`ng/exec/gpu/mod.rs:109-119` builds every input `TileView`
// with a zero-origin ROI); the CPU tiling path is the one that really
// carries an offset, and it uses `ctx.out_roi` directly.
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only
// output storage @group(1), params UBO @group(2).

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    amount: f32,
    scale_x: f32,
    scale_y: f32,
    mid: f32,
    feather: f32,
    highlights: f32,
    canvas_w: f32,
    canvas_h: f32,
    _pad0: f32,
    off_x: i32,
    off_y: i32,
    _pad1: i32,
    _pad2: i32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

const HL_LOW: f32 = 0.5;
const HL_HIGH: f32 = 1.0;

fn srgb_oetf(u: f32) -> f32 {
    let c = clamp(u, 0.0, 1.0);
    if c <= 0.0031308 {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn encoded_luma_of(c: vec4<f32>) -> f32 {
    let l = c.r * params.lw_r + c.g * params.lw_g + c.b * params.lw_b;
    return srgb_oetf(l);
}

fn smoothstep_01(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let lx = i32(gid.x);
    let ly = i32(gid.y);
    // Absolute canvas position of this pixel (see the header note on
    // `off_x`/`off_y`).
    let ax = f32(lx + params.off_x);
    let ay = f32(ly + params.off_y);

    // Centred normalized coordinates: [-1, 1] across the post-crop canvas,
    // sampled at the pixel centre so the falloff is symmetric about the
    // canvas centre for both even and odd extents.
    let u = ((ax + 0.5) / params.canvas_w) * 2.0 - 1.0;
    let v = ((ay + 0.5) / params.canvas_h) * 2.0 - 1.0;

    let du = u * params.scale_x;
    let dv = v * params.scale_y;
    let d = sqrt(du * du + dv * dv);
    let t = smoothstep_01(params.mid - params.feather, params.mid + params.feather, d);

    let c = textureLoad(src, vec2<i32>(lx, ly), 0);

    // `gain == 1.0` EXACTLY at `amount == 0` (`1.0 + 0.0 * t` is exact in
    // IEEE-754 for any finite `t`), which is what makes `is_identity`'s
    // elision contract a bit-exact identity rather than an approximation.
    let gain = max(1.0 + (params.amount / 100.0) * t, 0.0);

    // Highlight protection: only ever applies while the vignette is
    // DARKENING (gain < 1). This is the control that stops a strong vignette
    // crushing a bright sky; at `highlights == 100` a fully blown pixel is
    // left completely untouched.
    var protect: f32 = 0.0;
    if gain < 1.0 {
        let luma_e = encoded_luma_of(c);
        protect = (params.highlights / 100.0) * smoothstep_01(HL_LOW, HL_HIGH, luma_e);
    }
    let g = gain + (1.0 - gain) * protect;

    var outc = c.rgb * g;
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst, vec2<i32>(lx, ly), vec4<f32>(outc, c.a));
}
