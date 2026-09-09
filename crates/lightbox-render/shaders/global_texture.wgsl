// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_texture.wgsl, `global.texture` (E10 task D4). Mid-frequency
// band-pass boost/suppress on companion-encoded luma (spec §4.2 "Clarity /
// texture | ... band-pass (texture) on companion-encoded luma, detail
// recombined"): a 2-level box-blur pyramid band (`blur(R1) - blur(R2)`,
// `R1 < R2`) isolates a mid-frequency detail band, finer than `R1`
// (single-pixel grain) is excluded by construction (both blurs already
// smooth it away), coarser than `R2` (broad tonal shape) cancels out in the
// subtraction, the "2-level pyramid band" the D4 AC asks for.
//
// Single dispatch: `R1`'s box sum is a strict sub-window of `R2`'s (same
// center, `R1 < R2`), so both blurs are accumulated in ONE window loop
// (unlike `global_clarity.wgsl`'s two-pass guided-filter regression, which
// genuinely needs a first pass's output smoothed by a second). The same
// loop also tracks the window's min/max for the halo guard
// (`global_clarity.wgsl`'s "no new local extremum" technique, reused here
// verbatim, see that file's `pass_c` comment for the full rationale).
//
// Reapply is the identical local-derivative additive delta
// `global_clarity.wgsl` uses (`delta_linear = delta_encoded *
// d(srgb_eotf)/de`), for the identical reason: `texture == 0` must be an
// EXACT pixel identity even for out-of-gamut scene-linear pixels, which a
// clamped `companion_encode`/`companion_decode` round trip would not give.

const R1: i32 = 2;
const R2: i32 = 6;
const TEXTURE_RANGE: f32 = 1.0;
const GAIN_MIN: f32 = 0.0;
const GAIN_MAX: f32 = 2.0;

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    texture: f32,
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

fn srgb_eotf_deriv(e: f32) -> f32 {
    let c = clamp(e, 0.0, 1.0);
    if c <= 0.040449936 {
        return 1.0 / 12.92;
    }
    return (2.4 / 1.055) * pow((c + 0.055) / 1.055, 1.4);
}

fn encoded_luma_of(c: vec4<f32>) -> f32 {
    let l = c.r * params.lw_r + c.g * params.lw_g + c.b * params.lw_b;
    return srgb_oetf(l);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let dims_src = textureDimensions(src);
    let w = i32(dims_src.x);
    let h = i32(dims_src.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);

    var sum_r1: f32 = 0.0;
    var sum_r2: f32 = 0.0;
    var local_min: f32 = 1.0e30;
    var local_max: f32 = -1.0e30;
    for (var dy: i32 = -R2; dy <= R2; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -R2; dx <= R2; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let tap = encoded_luma_of(textureLoad(src, vec2<i32>(sx, sy), 0));
            sum_r2 = sum_r2 + tap;
            if abs(dx) <= R1 && abs(dy) <= R1 {
                sum_r1 = sum_r1 + tap;
            }
            local_min = min(local_min, tap);
            local_max = max(local_max, tap);
        }
    }
    let n1 = f32((2 * R1 + 1) * (2 * R1 + 1));
    let n2 = f32((2 * R2 + 1) * (2 * R2 + 1));
    let blur_r1 = sum_r1 / n1;
    let blur_r2 = sum_r2 / n2;
    let band = blur_r1 - blur_r2;

    let c = textureLoad(src, vec2<i32>(cx, cy), 0);
    let luma_e = encoded_luma_of(c);
    let gain = clamp(1.0 + (params.texture / 100.0) * TEXTURE_RANGE, GAIN_MIN, GAIN_MAX);
    let raw_delta_encoded = band * (gain - 1.0);
    let boosted = luma_e + raw_delta_encoded;
    let clamped = clamp(boosted, local_min, local_max);
    let delta_encoded = clamped - luma_e;
    let delta_linear = delta_encoded * srgb_eotf_deriv(luma_e);

    var outc = c.rgb + vec3<f32>(delta_linear, delta_linear, delta_linear);
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst, vec2<i32>(cx, cy), vec4<f32>(outc, c.a));
}
