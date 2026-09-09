// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// resize.wgsl, util.resize box (area-average) decimation (E05 §1 / task C2
// precursor; A-gpu). Turns a working-format input tile into a smaller
// working-format output tile for the scale ladders. Phase A ships the box
// filter; real Lanczos is C2. The CPU path (`decimate_box_cpu`) mirrors the
// same integer region math for the ΔE2000 parity gate (§4.4).
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only output
// storage @group(1), params UBO @group(2).

struct Params {
    in_w: u32,
    in_h: u32,
    out_w: u32,
    out_h: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.out_w || gid.y >= params.out_h {
        return;
    }
    // The half-open input region this output pixel covers. Guaranteed at least
    // one texel wide/tall even when out > in (identity/upscale degenerates to a
    // nearest tap), which keeps the average well-defined.
    let x0 = (gid.x * params.in_w) / params.out_w;
    let y0 = (gid.y * params.in_h) / params.out_h;
    let x1 = max(x0 + 1u, ((gid.x + 1u) * params.in_w) / params.out_w);
    let y1 = max(y0 + 1u, ((gid.y + 1u) * params.in_h) / params.out_h);

    var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var count = 0.0;
    for (var yy = y0; yy < y1; yy = yy + 1u) {
        for (var xx = x0; xx < x1; xx = xx + 1u) {
            acc = acc + textureLoad(src, vec2<i32>(i32(xx), i32(yy)), 0);
            count = count + 1.0;
        }
    }
    textureStore(dst, vec2<i32>(gid.xy), acc / max(count, 1.0));
}
