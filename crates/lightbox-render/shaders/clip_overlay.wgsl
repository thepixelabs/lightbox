// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// clip_overlay.wgsl, E10 Phase D task D13's J-key canvas clip-overlay pass.
// Tints highlight-clipped pixels (any of r/g/b == 255) solid red and
// shadow-clipped pixels (any of r/g/b == 0) solid blue, the conventional
// photo-editor "clipping display", over the RGBA8 display-space `src`
// texture (the canvas's published `rgba8unorm` frame); every other pixel
// passes through unchanged. A pixel that is simultaneously highlight- and
// shadow-clipped on different channels (e.g. pure red 255/0/0) resolves to
// highlight (checked first), a documented, deterministic tie-break, not an
// ambiguity.
//
// The CPU parity twin is `nodes::global::clip_overlay::clip_overlay_pixel`
// (identical `== 255` / `== 0` thresholds, identical tie-break), so this
// kernel and the CPU reference agree pixel-exactly on any input, the D13
// "overlays match ClipStats thresholds pixel-exactly" acceptance criterion.
// Same r8/g8/b0 recovery technique as `histogram_reduce.wgsl` (see that
// shader's module doc for why the unorm8<->float round trip is exact).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let r8 = u32(round(clamp(c.r, 0.0, 1.0) * 255.0));
    let g8 = u32(round(clamp(c.g, 0.0, 1.0) * 255.0));
    let b8 = u32(round(clamp(c.b, 0.0, 1.0) * 255.0));

    var out = c;
    if r8 == 255u || g8 == 255u || b8 == 255u {
        out = vec4<f32>(1.0, 0.0, 0.0, c.a);
    } else if r8 == 0u || g8 == 0u || b8 == 0u {
        out = vec4<f32>(0.0, 0.0, 1.0, c.a);
    }
    textureStore(dst, vec2<i32>(gid.xy), out);
}
