// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_bw_mix.wgsl, `global.bw_mix` (E10 task D1). Band-weighted
// black-and-white conversion: the 8-band raised-cosine hue weights (spec §4.2
// "HSL / vibrance / B&W mix | OkLCh derived from working RGB; raised-cosine
// hue-band weights") drive a per-band luminance-contribution mix, exactly as
// `nodes/common/colorspace.rs::band_weights`, the "shared C7 weights" this
// node reuses rather than re-deriving. Alpha passes through unchanged.
//
// Bind groups: input @group(0), write-only output storage @group(1), one
// flat `data` storage buffer @group(2) carrying (in order):
//   [0..3)   working-space luma weights (r,g,b), `working_luma_weights()`.
//   [3..39)  4 flattened 3x3 Oklab matrices (row-major): work_to_lms,
//            lms_to_work, lms_to_lab, lab_to_lms, `colorspace::matrices_flat()`.
//   [39..47) 8 hue-band boundary positions (degrees).
//   [47..55) 8 hue-band boundary transition half-widths (degrees).
//            (39..55 together = `colorspace::hue_band_geometry_flat()`.)
//   [55..63) 8 x mix weight (`-100..=100` each).
// This is the exact CPU-computed data `nodes::global::bw_mix::BwMixNode::
// eval_gpu` uploads, so this kernel never re-derives the matrices/geometry
// itself, only the per-pixel formula is duplicated (the established
// convention `global_hsl.wgsl` already uses for the same shared data shape).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<storage, read> data: array<f32>;

// Slider-to-gain calibration (kept in exact sync with the Rust-side
// `MIX_CHROMA_SCALE` in `nodes/global/bw_mix.rs`).
const MIX_CHROMA_SCALE: f32 = 1.0;

fn mat_vec3(base: u32, v: vec3<f32>) -> vec3<f32> {
    let r0 = vec3<f32>(data[base + 0u], data[base + 1u], data[base + 2u]);
    let r1 = vec3<f32>(data[base + 3u], data[base + 4u], data[base + 5u]);
    let r2 = vec3<f32>(data[base + 6u], data[base + 7u], data[base + 8u]);
    return vec3<f32>(dot(r0, v), dot(r1, v), dot(r2, v));
}

fn scbrt(x: f32) -> f32 {
    return sign(x) * pow(abs(x), 1.0 / 3.0);
}

fn working_to_oklch(rgb: vec3<f32>) -> vec3<f32> {
    let lms = mat_vec3(3u, rgb);
    let lms_ = vec3<f32>(scbrt(lms.x), scbrt(lms.y), scbrt(lms.z));
    let lab = mat_vec3(21u, lms_);
    let c = length(vec2<f32>(lab.y, lab.z));
    var h = degrees(atan2(lab.z, lab.y));
    if h < 0.0 {
        h = h + 360.0;
    }
    return vec3<f32>(lab.x, c, h);
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
    let t_prev = boundary_ramp(h, data[39u + prev_i], data[47u + prev_i]);
    let t_i = boundary_ramp(h, data[39u + i], data[47u + i]);
    return t_prev * (1.0 - t_i);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let luma_w = vec3<f32>(data[0], data[1], data[2]);
    let luma = dot(c.rgb, luma_w);
    let lch = working_to_oklch(c.rgb);

    var boost = 0.0;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let w = band_weight(i, lch.z);
        boost = boost + w * (data[55u + i] / 100.0);
    }
    let gain = max(1.0 + boost * MIX_CHROMA_SCALE * lch.y, 0.0);
    let gray = max(luma * gain, 0.0);
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(gray, gray, gray, c.a));
}
