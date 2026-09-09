// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_white_balance.wgsl, `global.wb` (E10 task A9). Applies a 3×3
// row-major matrix (the resolved white-balance transform, spec §4.2
// "WB (non-raw) | working space, Bradford/CAT02 adaptation") to working-space
// RGB. Alpha passes through unchanged. The CPU parity twin is
// `nodes::global::white_balance::apply_wb_matrix` (identical row·vector
// formula against the identical 9 params), so CPU/GPU parity (§4.4) holds to
// float rounding.
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2). The matrix is uploaded as three
// vec4 rows (std140 alignment); the 4th component of each row is unused
// padding.

struct Params {
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let r = dot(params.row0.xyz, c.rgb);
    let g = dot(params.row1.xyz, c.rgb);
    let b = dot(params.row2.xyz, c.rgb);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(r, g, b, c.a));
}
