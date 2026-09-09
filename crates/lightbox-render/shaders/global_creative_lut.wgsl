// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_creative_lut.wgsl, `global.creative_lut` (E10 task D9). Tetrahedral
// sampling of a 3D creative-look LUT, applied in the companion-encoded domain
// (spec §4.2/§4.8: "these packs are authored display-referred"), blended
// toward identity by `amount` (0 = identity, 1 = full, up to 2 = linear
// extrapolation past full strength, clamped in-gamut).
//
// The tetrahedral branch structure here is the EXACT mirror of
// `nodes::common::lut3d::Lut3D::sample_tetrahedral` (the classic
// Kasson/Spaulding six-tetrahedra split of the unit cube), reading from the
// SAME table layout `Lut3D::to_padded_rgba_f32` uploads (r-fastest -> x,
// g -> y, b-slowest -> z), so CPU/GPU parity (§4.4) holds by construction
// no LUT math is duplicated with a different formula on either side.
//
// Bind groups (§3.2): input @group(0), write-only output storage @group(1),
// params UBO @group(2), the 3D LUT texture @group(3) (this node's "LUT-aux"
// slot, the exact peer of `global_tone_curve.wgsl`'s @group(3) storage
// buffers).

struct Params {
    amount: f32,
    size: f32,
    _pad0: f32,
    _pad1: f32,
    domain_min: vec4<f32>,
    domain_max: vec4<f32>,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;
@group(3) @binding(0) var lut_tex: texture_3d<f32>;

fn srgb_oetf(u: f32) -> f32 {
    let c = clamp(u, 0.0, 1.0);
    if c <= 0.0031308 {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn srgb_eotf(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    if c <= 0.040449936 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn lut_at(i: i32, j: i32, k: i32) -> vec3<f32> {
    return textureLoad(lut_tex, vec3<i32>(i, j, k), 0).rgb;
}

// The exact mirror of `Lut3D::sample_tetrahedral` (see that fn's doc for the
// six-tetrahedra derivation): normalizes `rgb` from `[domain_min,domain_max]`
// to `[0,1]`, maps to continuous table coordinates, then combines the 4
// tetrahedron corners enclosing the fractional position with weights that
// sum to exactly 1 in every branch.
fn tetrahedral_sample(rgb: vec3<f32>) -> vec3<f32> {
    let n = i32(params.size);
    if n < 2 {
        return rgb;
    }
    let nm1 = f32(n - 1);
    let dmin = params.domain_min.xyz;
    let dmax = params.domain_max.xyz;
    let span = max(dmax - dmin, vec3<f32>(1.0e-6));
    let norm = clamp((rgb - dmin) / span, vec3<f32>(0.0), vec3<f32>(1.0));
    let p = norm * nm1;
    let i0 = clamp(i32(floor(p.x)), 0, n - 2);
    let j0 = clamp(i32(floor(p.y)), 0, n - 2);
    let k0 = clamp(i32(floor(p.z)), 0, n - 2);
    let f = p - vec3<f32>(f32(i0), f32(j0), f32(k0));
    let fx = f.x;
    let fy = f.y;
    let fz = f.z;

    let c000 = lut_at(i0, j0, k0);
    let c100 = lut_at(i0 + 1, j0, k0);
    let c010 = lut_at(i0, j0 + 1, k0);
    let c001 = lut_at(i0, j0, k0 + 1);
    let c110 = lut_at(i0 + 1, j0 + 1, k0);
    let c101 = lut_at(i0 + 1, j0, k0 + 1);
    let c011 = lut_at(i0, j0 + 1, k0 + 1);
    let c111 = lut_at(i0 + 1, j0 + 1, k0 + 1);

    var out: vec3<f32>;
    if fx > fy {
        if fy > fz {
            out = (1.0 - fx) * c000 + (fx - fy) * c100 + (fy - fz) * c110 + fz * c111;
        } else if fx > fz {
            out = (1.0 - fx) * c000 + (fx - fz) * c100 + (fz - fy) * c101 + fy * c111;
        } else {
            out = (1.0 - fz) * c000 + (fz - fx) * c001 + (fx - fy) * c101 + fy * c111;
        }
    } else if fz > fy {
        out = (1.0 - fz) * c000 + (fz - fy) * c001 + (fy - fx) * c011 + fx * c111;
    } else if fz > fx {
        out = (1.0 - fy) * c000 + (fy - fz) * c010 + (fz - fx) * c011 + fx * c111;
    } else {
        out = (1.0 - fy) * c000 + (fy - fx) * c010 + (fx - fz) * c110 + fz * c111;
    }
    return out;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let enc = vec3<f32>(srgb_oetf(c.r), srgb_oetf(c.g), srgb_oetf(c.b));
    let mapped = tetrahedral_sample(enc);

    let amt = params.amount;
    var blended: vec3<f32>;
    if amt <= 1.0 {
        blended = mix(enc, mapped, clamp(amt, 0.0, 1.0));
    } else {
        // Linear extrapolation past full strength (amount in 1..=2),
        // clamped in-gamut below.
        let extra = amt - 1.0;
        blended = mapped + extra * (mapped - enc);
    }
    blended = clamp(blended, vec3<f32>(0.0), vec3<f32>(1.0));

    let dec = vec3<f32>(srgb_eotf(blended.x), srgb_eotf(blended.y), srgb_eotf(blended.z));
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(dec, clamp(c.a, 0.0, 1.0)));
}
