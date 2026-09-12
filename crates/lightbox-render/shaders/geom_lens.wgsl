// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// geom_lens.wgsl, `geom.lens`: radial lens-distortion correction plus radial
// lens-vignetting correction, in one pass. Both are functions of the same
// normalized radius about the same frame centre, so they share the pass; the
// per-channel optics (lateral CA, defringe) cannot, and live in
// `geom_defringe.wgsl`.
//
// Coordinate convention is `ng::warp::field`'s, verbatim: pixel `(x, y)` sits
// at the exact continuous coordinate `(x, y)` (not `x+0.5`), and the frame
// centre is `((w-1)/2, (h-1)/2)`. `geom_warp.wgsl` maps destination to source
// under that same convention (see its lines 53-63), so this pass and the warp
// pass agree about pixel centres and composing them introduces no half-pixel
// drift. The `sample_lanczos3` below is the same Lanczos-3 + 2x2
// anti-ringing gather, tap for tap and weight for weight, that
// `geom_warp.wgsl` writes inline in its `main` (lines 69-100), for the same
// reason. WGSL has no include, so it is repeated rather than shared; the CPU
// side does share one function.
//
// Distortion model (destination radius to source radius, the direction an
// inverse-mapped resampler needs):
//
//     scale(rn) = 1 + k1*rn^2 + k2*rn^4 + k3*rn^6
//     src       = centre + (dst - centre) * scale(rn)
//
// with `rn` the destination point's distance from the centre divided by half
// the frame diagonal, so `rn == 1` exactly at the corner pixels. Negative k1
// samples inside the destination radius, which pushes content outward and so
// removes barrel distortion; positive k1 removes pincushion.
//
// Vignetting model: a radial gain applied to RGB (never alpha),
//
//     gain(rs) = 1 + a*rs^2 + b*rs^4 + c*rs^6
//
// evaluated at the SOURCE radius `rs`, not the destination radius: lens
// falloff is a property of the captured frame, and the distortion resample
// has already told us which source radius this destination pixel came from.
//
// The CPU parity twin is `nodes::geometry::lens`.
//
// Bind groups follow the shaders/README.md conventions: input @group(0),
// write-only output storage @group(1), params UBO @group(2).

struct Params {
    k1: f32,
    k2: f32,
    k3: f32,
    vig_a: f32,
    vig_b: f32,
    vig_c: f32,
    center_x: f32,
    center_y: f32,
    inv_r_norm: f32,
    in_w: u32,
    in_h: u32,
    in_off_x: i32,
    in_off_y: i32,
    distort_on: u32,
    vignette_on: u32,
    _pad0: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

const LANCZOS_A: f32 = 3.0;
const PI: f32 = 3.14159265358979323846;

fn lanczos3(x: f32) -> f32 {
    let ax = abs(x);
    if ax < 1e-6 {
        return 1.0;
    }
    if ax < LANCZOS_A {
        let px = PI * x;
        return LANCZOS_A * sin(px) * sin(px / LANCZOS_A) / (px * px);
    }
    return 0.0;
}

// 6x6-tap Lanczos-3 gather at the continuous input-local coordinate
// `(fx, fy)`, edge-clamped, then clamped to the local 2x2 neighborhood's
// per-channel min/max (anti-ringing). Identical to `geom_warp.wgsl`'s body.
fn sample_lanczos3(fx: f32, fy: f32) -> vec4<f32> {
    let x0 = i32(floor(fx)) - 2;
    let y0 = i32(floor(fy)) - 2;
    var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var wsum: f32 = 0.0;
    var lo = vec4<f32>(3.4e38, 3.4e38, 3.4e38, 3.4e38);
    var hi = vec4<f32>(-3.4e38, -3.4e38, -3.4e38, -3.4e38);
    let max_x = i32(params.in_w) - 1;
    let max_y = i32(params.in_h) - 1;

    for (var j: i32 = 0; j < 6; j = j + 1) {
        let sy_i = clamp(y0 + j, 0, max(max_y, 0));
        let wy = lanczos3(fy - f32(y0 + j));
        for (var i: i32 = 0; i < 6; i = i + 1) {
            let sx_i = clamp(x0 + i, 0, max(max_x, 0));
            let wx = lanczos3(fx - f32(x0 + i));
            let wgt = wx * wy;
            let p = textureLoad(src, vec2<i32>(sx_i, sy_i), 0);
            acc = acc + wgt * p;
            wsum = wsum + wgt;
            if i >= 2 && i <= 3 && j >= 2 && j <= 3 {
                lo = min(lo, p);
                hi = max(hi, p);
            }
        }
    }
    var result: vec4<f32>;
    if abs(wsum) > 1e-8 {
        result = acc / wsum;
    } else {
        result = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return clamp(result, min(lo, hi), max(lo, hi));
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let ax = f32(gid.x);
    let ay = f32(gid.y);
    let dx = ax - params.center_x;
    let dy = ay - params.center_y;
    let rn = sqrt(dx * dx + dy * dy) * params.inv_r_norm;

    var scale: f32 = 1.0;
    if params.distort_on != 0u {
        let r2 = rn * rn;
        scale = 1.0 + r2 * (params.k1 + r2 * (params.k2 + r2 * params.k3));
    }
    let fx = params.center_x + dx * scale - f32(params.in_off_x);
    let fy = params.center_y + dy * scale - f32(params.in_off_y);

    var rgba: vec4<f32>;
    if params.distort_on != 0u {
        rgba = sample_lanczos3(fx, fy);
    } else {
        // Distortion off: `fx`/`fy` are exactly the input-local integer
        // coordinates, so a plain clamped fetch is the bit-exact answer and
        // skips 36 taps of Lanczos that would only reproduce it.
        let ix = clamp(i32(fx), 0, max(i32(params.in_w) - 1, 0));
        let iy = clamp(i32(fy), 0, max(i32(params.in_h) - 1, 0));
        rgba = textureLoad(src, vec2<i32>(ix, iy), 0);
    }

    if params.vignette_on != 0u {
        let rs = rn * scale;
        let s2 = rs * rs;
        let gain = max(
            0.0,
            1.0 + s2 * (params.vig_a + s2 * (params.vig_b + s2 * params.vig_c)),
        );
        rgba = vec4<f32>(rgba.rgb * gain, rgba.a);
    }

    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), rgba);
}
