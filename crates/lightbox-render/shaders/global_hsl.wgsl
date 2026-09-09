// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_hsl.wgsl, `global.hsl` (E10 task C8). 8-band HSL color mixer,
// evaluated in OkLCh derived from working RGB (spec §4.2). Own, clean-room
// implementation of Björn Ottosson's published Oklab formulas (same
// matrices `nodes/common/colorspace.rs` computes on the CPU side and
// uploads here verbatim via `data`, see that module's doc comment for the
// clean-room/provenance note). Alpha passes through unchanged.
//
// Bind groups: input @group(0), write-only output storage @group(1), one
// flat `data` storage buffer @group(2) carrying (in order):
//   [0..36)  4 flattened 3x3 matrices (row-major): work_to_lms, lms_to_work,
//            lms_to_lab, lab_to_lms, `colorspace::matrices_flat()`.
//   [36..44) 8 hue-band boundary positions (degrees).
//   [44..52) 8 hue-band boundary transition half-widths (degrees).
//            (36..52 together = `colorspace::hue_band_geometry_flat()`.)
//   [52..76) 8 x [hue, sat, lum] band params (`-100..=100` each).
// This is the exact CPU-computed data `nodes::global::hsl::HslNode::eval_gpu`
// uploads, so this kernel never re-derives the matrices/geometry itself
// only the per-pixel formula is duplicated (same convention
// `global_whites_blacks.wgsl` uses for its sRGB OETF/EOTF constants).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<storage, read> data: array<f32>;

const HUE_RANGE_DEG: f32 = 30.0;
const SAT_RANGE: f32 = 1.0;
const LUM_RANGE: f32 = 0.20;

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

fn boundary_ramp(h: f32, boundary: f32, hw: f32) -> f32 {
    return smoothstep(-hw, hw, circ_delta(h, boundary));
}

fn band_weight(i: u32, h: f32) -> f32 {
    let prev_i = (i + 7u) % 8u;
    let t_prev = boundary_ramp(h, data[36u + prev_i], data[44u + prev_i]);
    let t_i = boundary_ramp(h, data[36u + i], data[44u + i]);
    return t_prev * (1.0 - t_i);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let lch = working_to_oklch(c.rgb);

    var hue_shift = 0.0;
    var sat_sum = 0.0;
    var lum_sum = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(i, lch.z);
        let base = 52u + i * 3u;
        hue_shift = hue_shift + w * data[base + 0u];
        sat_sum = sat_sum + w * data[base + 1u];
        lum_sum = lum_sum + w * data[base + 2u];
    }

    let new_h = lch.z + (hue_shift / 100.0) * HUE_RANGE_DEG;
    let sat_scale = max(1.0 + (sat_sum / 100.0) * SAT_RANGE, 0.0);
    let new_c = max(lch.y * sat_scale, 0.0);
    let new_l = lch.x + (lum_sum / 100.0) * LUM_RANGE;

    let out_rgb = oklch_to_working(vec3<f32>(new_l, new_c, new_h));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(out_rgb, c.a));
}
