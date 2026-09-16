// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// geom_defringe.wgsl, `geom.defringe`: lateral chromatic-aberration
// correction and defringe. Two DIFFERENT operations, deliberately not
// conflated:
//
//   * lateral CA is geometric. Red and blue focus at slightly different
//     magnifications from green, so red and blue are resampled at a slightly
//     scaled radius about the frame centre and green is left where it is.
//     It moves pixels; it invents no colour.
//   * defringe is chromatic. It removes the purple/violet and green fringes
//     that survive alignment (longitudinal CA, sensor crosstalk, demosaic
//     ringing) by pulling the green-magenta opponent channel back toward its
//     local low-frequency level at high-contrast edges. It moves colour; it
//     moves no pixels.
//
// Both belong before the distortion resample in `geom_lens.wgsl`: resampling
// smears the very fringe these are trying to isolate.
//
// Coordinate convention is `ng::warp::field`'s, verbatim: pixel `(x, y)` at
// exact continuous `(x, y)`, frame centre `((w-1)/2, (h-1)/2)`. The CA
// resample uses the same Lanczos-3 + 2x2 anti-ringing kernel as
// `geom_warp.wgsl` and `geom_lens.wgsl`.
//
// CA model:
//
//     R_out(p) = R_in(centre + (p - centre) * (1 + s_r))
//     B_out(p) = B_in(centre + (p - centre) * (1 + s_b))
//     G_out(p) = G_in(p)
//
// `s_r`/`s_b` are per-channel radial scale deltas. They are measured
// per-lens quantities; see `nodes::geometry::lens::resolve_lens_profile` for
// where they come from and why they are zero in this build.
//
// Defringe model, with `cd = G - (R + B) / 2` (the green-magenta opponent
// channel; purple fringes drive it negative, green fringes positive):
//
//     cd_ref  = mean of cd over a (2R+1)^2 window   (low-frequency level)
//     edge    = smoothstep(EDGE_LO, EDGE_HI, (Ymax - Ymin) / (Ymax + Ymin))
//     G_out   = G + amount * edge * (cd_ref - cd)
//
// Only the green-magenta opponent channel moves, which is the pair of hues
// defringe targets. That axis is not orthogonal to blue-yellow (blue lands
// on its magenta side), so what protects real object colour is the
// reference, not the axis: a genuinely coloured object has a LOW-frequency
// cd, so `cd_ref - cd` is near zero over its interior and the object
// survives, while a one to three pixel fringe is high-frequency cd at a
// high-contrast edge and is removed.
//
// The CPU parity twin is `nodes::geometry::defringe`.
//
// Bind groups follow the shaders/README.md conventions: input @group(0),
// write-only output storage @group(1), params UBO @group(2).

struct Params {
    ca_scale_r: f32,
    ca_scale_b: f32,
    amount: f32,
    center_x: f32,
    center_y: f32,
    in_w: u32,
    in_h: u32,
    in_off_x: i32,
    in_off_y: i32,
    ca_on: u32,
    defringe_on: u32,
    _pad0: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

const LANCZOS_A: f32 = 3.0;
const PI: f32 = 3.14159265358979323846;

// Defringe reference-window radius, in pixels. Three is the smallest radius
// whose window still spans a one to three pixel fringe plus clean pixels on
// both sides of it, which is what makes `cd_ref` a fringe-free reference.
// The CPU twin's `DEFRINGE_RADIUS` must match.
const DEFRINGE_RADIUS: i32 = 3;

// Local-contrast gate. Below EDGE_LO no correction at all, above EDGE_HI the
// full `amount`. These are UI-shaping choices, not measured constants: they
// keep the control off in flat and gently shaded areas, where a colour
// difference is object colour rather than a fringe.
const EDGE_LO: f32 = 0.15;
const EDGE_HI: f32 = 0.50;

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

// 6x6-tap Lanczos-3 gather, edge-clamped, 2x2 anti-ringing clamped. Same
// taps and same weights as `geom_warp.wgsl`'s inline gather and
// `geom_lens.wgsl`'s copy of it; WGSL has no include, so it is repeated
// rather than shared (the CPU side does share one function).
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

fn luma(c: vec3<f32>) -> f32 {
    // Rec. 709 luminance over the scene-linear working RGB this node runs in.
    return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
}

fn opponent(c: vec3<f32>) -> f32 {
    return c.g - 0.5 * (c.r + c.b);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let ax = f32(gid.x);
    let ay = f32(gid.y);
    let lx = i32(gid.x) - params.in_off_x;
    let ly = i32(gid.y) - params.in_off_y;
    let max_x = max(i32(params.in_w) - 1, 0);
    let max_y = max(i32(params.in_h) - 1, 0);

    var rgba = textureLoad(
        src,
        vec2<i32>(clamp(lx, 0, max_x), clamp(ly, 0, max_y)),
        0,
    );

    // ── lateral CA: per-channel radial rescale ──────────────────────────────
    if params.ca_on != 0u {
        let dx = ax - params.center_x;
        let dy = ay - params.center_y;
        let base_x = params.center_x - f32(params.in_off_x);
        let base_y = params.center_y - f32(params.in_off_y);
        let sr = 1.0 + params.ca_scale_r;
        let sb = 1.0 + params.ca_scale_b;
        let red = sample_lanczos3(base_x + dx * sr, base_y + dy * sr);
        let blue = sample_lanczos3(base_x + dx * sb, base_y + dy * sb);
        rgba = vec4<f32>(red.r, rgba.g, blue.b, rgba.a);
    }

    // ── defringe: edge-gated green-magenta opponent suppression ─────────────
    if params.defringe_on != 0u {
        var cd_sum: f32 = 0.0;
        var n: f32 = 0.0;
        var y_min: f32 = 3.4e38;
        var y_max: f32 = -3.4e38;
        for (var j: i32 = -DEFRINGE_RADIUS; j <= DEFRINGE_RADIUS; j = j + 1) {
            let sy = clamp(ly + j, 0, max_y);
            for (var i: i32 = -DEFRINGE_RADIUS; i <= DEFRINGE_RADIUS; i = i + 1) {
                let sx = clamp(lx + i, 0, max_x);
                // Detection runs on the pixel as it would display, held to
                // [0, 1], so a recovered above-white highlight reads as white
                // with an opponent of zero rather than as a magenta fringe.
                // Mirrors `as_displayed` in defringe.rs.
                let p = clamp(textureLoad(src, vec2<i32>(sx, sy), 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
                cd_sum = cd_sum + opponent(p);
                n = n + 1.0;
                let y = luma(p);
                y_min = min(y_min, y);
                y_max = max(y_max, y);
            }
        }
        // The reference level is read off the node's INPUT, before this
        // pass's own CA rescale. That is deliberate and it is an
        // approximation: the reference is a low-pass, and a lateral-CA shift
        // is a few pixels at most, so shifting it changes it only by the
        // window's own (small) low-frequency gradient. The centre value the
        // reference is compared against is the CA-corrected one, which is
        // exact.
        let cd_ref = cd_sum / max(n, 1.0);
        let cd = opponent(clamp(rgba.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
        let contrast = (y_max - y_min) / max(y_max + y_min, 1e-6);
        let edge = smoothstep(EDGE_LO, EDGE_HI, contrast);
        let delta = params.amount * edge * (cd_ref - cd);
        // Floor at zero only when the correction pulls green down; an
        // untouched pixel keeps its value, negative out-of-gamut included.
        let g = rgba.g + delta;
        rgba = vec4<f32>(rgba.r, select(g, max(g, 0.0), delta < 0.0), rgba.b, rgba.a);
    }

    textureStore(dst, vec2<i32>(i32(gid.x), i32(gid.y)), rgba);
}
