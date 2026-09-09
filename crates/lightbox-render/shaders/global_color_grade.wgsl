// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_color_grade.wgsl, `global.color_grade` (E10 tasks C11-C12). Three-way
// (shadows/midtones/highlights) + global colour-grading wheels, evaluated in
// OkLCh derived from working RGB (spec §4.2). Own, clean-room implementation
// of Björn Ottosson's published Oklab formulas (same matrices
// `nodes/common/colorspace.rs` computes on the CPU side and uploads here
// verbatim via `data`, see that module's doc comment for the clean-room/
// provenance note, and `global_hsl.wgsl`'s header for the shared convention).
// Alpha passes through unchanged.
//
// Bind groups: input @group(0), write-only output storage @group(1), one
// flat `data` storage buffer @group(2) carrying (in order):
//   [0..36)  4 flattened 3x3 matrices (row-major): work_to_lms, lms_to_work,
//            lms_to_lab, lab_to_lms, `colorspace::matrices_flat()`.
//   [36..39) working-space CIE Y luma weights (R,G,B)
//            `tone_recovery::working_luma_weights()`.
//   [39]     blending (0..=100)
//   [40]     balance (-100..=100)
//   [41..53) 4 x [hue_deg, sat, lum] wheels: shadows, midtones, highlights,
//            global.
// This is the exact CPU-computed data
// `nodes::global::color_grade::ColorGradeNode::eval_gpu` uploads, so this
// kernel never re-derives the matrices/luma-weights themselves, only the
// per-pixel formula (incl. the sRGB-shaped companion encode used for the
// luma-zone weights) is duplicated (same convention
// `global_whites_blacks.wgsl` uses for its sRGB OETF/EOTF constants).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<storage, read> data: array<f32>;

const BASE_BOUNDARY_LOW: f32 = 0.333333;
const BASE_BOUNDARY_HIGH: f32 = 0.666667;
const BALANCE_RANGE: f32 = 0.25;
const BLEND_HW_MIN: f32 = 0.03;
const BLEND_HW_MAX: f32 = 0.28;
const GRADE_CHROMA_RANGE: f32 = 0.11;
const GRADE_LUM_RANGE: f32 = 0.15;

fn mat_vec3(base: u32, v: vec3<f32>) -> vec3<f32> {
    let r0 = vec3<f32>(data[base + 0u], data[base + 1u], data[base + 2u]);
    let r1 = vec3<f32>(data[base + 3u], data[base + 4u], data[base + 5u]);
    let r2 = vec3<f32>(data[base + 6u], data[base + 7u], data[base + 8u]);
    return vec3<f32>(dot(r0, v), dot(r1, v), dot(r2, v));
}

fn scbrt(x: f32) -> f32 {
    return sign(x) * pow(abs(x), 1.0 / 3.0);
}

fn working_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = mat_vec3(0u, rgb);
    let lms_ = vec3<f32>(scbrt(lms.x), scbrt(lms.y), scbrt(lms.z));
    return mat_vec3(18u, lms_);
}

fn oklab_to_working(lab: vec3<f32>) -> vec3<f32> {
    let lms_ = mat_vec3(27u, lab);
    let lms = lms_ * lms_ * lms_;
    return mat_vec3(9u, lms);
}

fn srgb_oetf(u: f32) -> f32 {
    let c = clamp(u, 0.0, 1.0);
    if c <= 0.0031308 {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn zone_weights(y_enc: f32, blending: f32, balance: f32) -> vec3<f32> {
    let shift = (balance / 100.0) * BALANCE_RANGE;
    let b0 = BASE_BOUNDARY_LOW + shift;
    let b1 = BASE_BOUNDARY_HIGH + shift;
    let gap = max(b1 - b0, 1e-4);
    let t = clamp(blending / 100.0, 0.0, 1.0);
    let hw = max(min(BLEND_HW_MIN + t * (BLEND_HW_MAX - BLEND_HW_MIN), 0.49 * gap), 1e-4);
    let t0 = smoothstep(b0 - hw, b0 + hw, y_enc);
    let t1 = smoothstep(b1 - hw, b1 + hw, y_enc);
    return vec3<f32>(1.0 - t0, t0 - t1, t1);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);

    let lw = vec3<f32>(data[36u], data[37u], data[38u]);
    let lin_luma = max(dot(c.rgb, lw), 0.0);
    let y_enc = srgb_oetf(lin_luma);

    let blending = data[39u];
    let balance = data[40u];
    let zw = zone_weights(y_enc, blending, balance);
    let weights = array<f32, 4>(zw.x, zw.y, zw.z, 1.0);

    let lab = working_to_oklab(c.rgb);
    var off_a = 0.0;
    var off_b = 0.0;
    var l_delta = 0.0;
    for (var i = 0u; i < 4u; i = i + 1u) {
        let w = weights[i];
        let base = 41u + i * 3u;
        let hue = data[base + 0u];
        let sat = data[base + 1u];
        let lum = data[base + 2u];
        let chroma = (sat / 100.0) * GRADE_CHROMA_RANGE;
        let hr = radians(hue);
        off_a = off_a + w * chroma * cos(hr);
        off_b = off_b + w * chroma * sin(hr);
        l_delta = l_delta + w * (lum / 100.0) * GRADE_LUM_RANGE;
    }

    let new_lab = vec3<f32>(lab.x + l_delta, lab.y + off_a, lab.z + off_b);
    let out_rgb = oklab_to_working(new_lab);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(out_rgb, c.a));
}
