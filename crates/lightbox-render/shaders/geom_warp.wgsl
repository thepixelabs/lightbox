// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// geom_warp.wgsl, `geom.warp` (E11 tasks E1/E2). Single-pass inverse-mapped
// Lanczos-3 resample over the composed WarpField (this slice: straighten
// `angle` only, see `ng::warp::field`'s module docs). Per-pixel exact
// analytic rotation (no per-tile grid interpolation, see
// `WarpField::to_gpu_uniform`'s doc comment and
// docs/plan/epics/E11-deviations.md). The CPU parity twin is
// `nodes::geometry::warp::sample_lanczos3` (identical Lanczos-3 + 2x2
// anti-ringing-clamp formula).
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2).

struct Params {
    sin_a: f32,
    cos_a: f32,
    center_x: f32,
    center_y: f32,
    in_w: u32,
    in_h: u32,
    in_off_x: i32,
    in_off_y: i32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

const LANCZOS_A: f32 = 3.0;
const PI: f32 = 3.14159265358979323846;

fn lanczos3(x: f32) -> f32 {
    let ax = abs(x);
    if ax < 1e-6 {
        return 1.0;
    }
    if ax < LANCZOS_A {
        let px = PI * x;
        return LANCZOS_A * sin(px) * sin(px / LANCZOS_A) / (px * px);
    }
    return 0.0;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    // Destination-canvas absolute coordinate == local gid in this engine's
    // current single-whole-tile production path (input and output share the
    // same frame; `in_off_*` is 0 there). Kept general for the reference
    // tiled-CPU-path parity story.
    let ax = f32(gid.x);
    let ay = f32(gid.y);
    let dx = ax - params.center_x;
    let dy = ay - params.center_y;
    // map_dst_to_src: rotate by -angle (see ng::warp::field's convention).
    let sx = params.center_x + dx * params.cos_a + dy * params.sin_a;
    let sy = params.center_y - dx * params.sin_a + dy * params.cos_a;

    // Into the (possibly apron-offset) input tile's local coordinates.
    let fx = sx - f32(params.in_off_x);
    let fy = sy - f32(params.in_off_y);

    let x0 = i32(floor(fx)) - 2;
    let y0 = i32(floor(fy)) - 2;
    var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var wsum: f32 = 0.0;
    var lo = vec4<f32>(3.4e38, 3.4e38, 3.4e38, 3.4e38);
    var hi = vec4<f32>(-3.4e38, -3.4e38, -3.4e38, -3.4e38);
    let max_x = i32(params.in_w) - 1;
    let max_y = i32(params.in_h) - 1;

    for (var j: i32 = 0; j < 6; j = j + 1) {
        let sy_i = clamp(y0 + j, 0, max(max_y, 0));
        let wy = lanczos3(fy - f32(y0 + j));
        for (var i: i32 = 0; i < 6; i = i + 1) {
            let sx_i = clamp(x0 + i, 0, max(max_x, 0));
            let wx = lanczos3(fx - f32(x0 + i));
            let wgt = wx * wy;
            let p = textureLoad(src, vec2<i32>(sx_i, sy_i), 0);
            acc = acc + wgt * p;
            wsum = wsum + wgt;
            if i >= 2 && i <= 3 && j >= 2 && j <= 3 {
                lo = min(lo, p);
                hi = max(hi, p);
            }
        }
    }
    var result: vec4<f32>;
    if abs(wsum) > 1e-8 {
        result = acc / wsum;
    } else {
        result = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    result = clamp(result, min(lo, hi), max(lo, hi));
    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), result);
}
