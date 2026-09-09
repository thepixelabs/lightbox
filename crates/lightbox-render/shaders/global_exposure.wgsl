// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_exposure.wgsl, `global.exposure` (E10 task A6). Scene-linear
// working-RGB gain: `gain = 2^ev` (spec §4.2 "exposure = scene-linear
// working, gain = 2^EV"). Alpha passes through unchanged. The CPU parity twin
// is `nodes::global::exposure::exposure_gain` (same formula, `f32::exp2`).
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2).

struct Params {
    ev: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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
    let gain = exp2(params.ev);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(c.rgb * gain, c.a));
}
