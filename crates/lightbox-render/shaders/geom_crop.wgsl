// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// geom_crop.wgsl, `geom.crop` (E11 task E4). A pure index copy (optionally
// row/column-reversed for `Flip`) selecting an axis-aligned pixel window of
// the input, never a resample. The CPU parity twin is
// `nodes::geometry::crop::GeomCropNode::eval_cpu` (identical index formula).
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2).

struct Params {
    left_px: i32,
    top_px: i32,
    out_w: u32,
    out_h: u32,
    flip_h: u32,
    flip_v: u32,
    in_off_x: i32,
    in_off_y: i32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.out_w || gid.y >= params.out_h {
        return;
    }
    let ox = i32(gid.x);
    let oy = i32(gid.y);

    var ux: i32;
    if params.flip_h != 0u {
        ux = params.left_px + (i32(params.out_w) - 1 - ox);
    } else {
        ux = params.left_px + ox;
    }
    var uy: i32;
    if params.flip_v != 0u {
        uy = params.top_px + (i32(params.out_h) - 1 - oy);
    } else {
        uy = params.top_px + oy;
    }

    let dims = textureDimensions(src);
    let ix = clamp(ux - params.in_off_x, 0, max(i32(dims.x) - 1, 0));
    let iy = clamp(uy - params.in_off_y, 0, max(i32(dims.y) - 1, 0));

    let c = textureLoad(src, vec2<i32>(ix, iy), 0);
    textureStore(dst, vec2<i32>(ox, oy), c);
}
