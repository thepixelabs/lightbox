// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// copy.wgsl, the src.decoded source-injection kernel (E05 §1 engine-owned
// nodes; A-gpu). Identity copy of a working-format input tile into the
// working-format output tile. `src.decoded` normally has its output provided by
// the uploader (the executor pre-populates the source stage), so this kernel is
// the defensive/tested copy path: input at @group(0), write-only output storage
// at @group(1) per the §3.2 bind-group conventions.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    textureStore(dst, vec2<i32>(gid.xy), c);
}
