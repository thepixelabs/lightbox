// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_vibrance_sat.wgsl, `global.vibrance_sat` (E10 task C10). Uniform
// saturation + chroma-weighted, skin-hue-protected vibrance, evaluated in
// OkLCh derived from working RGB (spec §4.2, same domain/matrices as
// `global_hsl.wgsl`, see that shader's header for the clean-room/provenance
// note on the Oklab matrices). Alpha passes through unchanged.
//
// Bind groups: input @group(0), write-only output storage @group(1), one
// flat `data` storage buffer @group(2) carrying (in order):
//   [0..36) 4 flattened 3x3 matrices (row-major): work_to_lms, lms_to_work,
//           lms_to_lab, lab_to_lms, `colorspace::matrices_flat()`.
//   [36]    vibrance (-100..=100)
//   [37]    saturation (-100..=100)
//   [38]    skin-hue protection band center (degrees)
//   [39]    skin-hue protection band half-width (degrees)
// This is the exact CPU-computed data `nodes::global::vibrance_sat::
// VibranceSatNode::eval_gpu` uploads.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<storage, read> data: array<f32>;

const SAT_UNIFORM_RANGE: f32 = 1.0;
const VIB_RANGE: f32 = 1.5;
const CHROMA_REF: f32 = 0.30;
const SKIN_PROTECT_STRENGTH: f32 = 0.85;

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

fn working_to_oklch(rgb: vec3<f32>) -> vec3<f32> {
    let lab = working_to_oklab(rgb);
    let c = length(vec2<f32>(lab.y, lab.z));
    var h = degrees(atan2(lab.z, lab.y));
    if h < 0.0 {
        h = h + 360.0;
    }
    return vec3<f32>(lab.x, c, h);
}

fn oklch_to_working(lch: vec3<f32>) -> vec3<f32> {
    let hr = radians(lch.z);
    return oklab_to_working(vec3<f32>(lch.x, lch.y * cos(hr), lch.y * sin(hr)));
}

fn circ_delta(h: f32, center: f32) -> f32 {
    var d = h - center;
    d = d - 360.0 * round(d / 360.0);
    return d;
}

fn skin_weight(hue_deg: f32, center: f32, half_width: f32) -> f32 {
    let d = abs(circ_delta(hue_deg, center));
    let hw = max(half_width, 1e-6);
    return 1.0 - smoothstep(0.0, hw, d);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let lch = working_to_oklch(c.rgb);

    let vibrance = data[36u];
    let saturation = data[37u];
    let skin_center = data[38u];
    let skin_half_width = data[39u];

    let sat_scale = max(1.0 + (saturation / 100.0) * SAT_UNIFORM_RANGE, 0.0);
    let chroma_weight = clamp(1.0 - lch.y / CHROMA_REF, 0.0, 1.0);
    let skin = skin_weight(lch.z, skin_center, skin_half_width);
    let vib_scale = 1.0 + (vibrance / 100.0) * VIB_RANGE * chroma_weight * (1.0 - skin * SKIN_PROTECT_STRENGTH);
    let new_c = max(lch.y * sat_scale * vib_scale, 0.0);

    let out_rgb = oklch_to_working(vec3<f32>(lch.x, new_c, lch.z));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(out_rgb, c.a));
}
