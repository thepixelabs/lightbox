// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_grain.wgsl, `fx.grain`: film grain from
// `lightbox_edit::leaves::Grain` (amount / size / roughness).
//
// # The noise field never moves
//
// The grain value at a pixel is a pure function of that pixel's ABSOLUTE
// canvas position and the two fixed seeds below. Nothing here reads a frame
// counter, a clock, or a per-dispatch random source, so dragging a slider
// re-renders the identical noise field and the grain sits still instead of
// crawling. The hash is integer-only (wrapping u32 multiplies and logical
// shifts, which WGSL and Rust agree on bit-for-bit), so the CPU reference in
// `nodes::global::grain` produces the same lattice values as this kernel
// rather than merely a statistically similar one.
//
// # The field
//
// Two octaves of smooth value noise. The fine octave's lattice spacing is
// `1 / inv_cell_fine` pixels (the `size` slider); the coarse octave, at
// `GRAIN_ROUGH_CELL_SCALE` times that spacing, modulates the fine octave's
// amplitude by `roughness`, which is what turns even grain into clumpy,
// uneven grain.
//
// # Applying it
//
// Grain is monochromatic (a luminance perturbation, like film), computed as
// a delta in the companion-encoded domain so its visual weight is uniform
// across the tone range, then converted to a scene-linear delta through the
// FIRST DERIVATIVE of the sRGB EOTF and added identically to R/G/B. That is
// the same reapply `global_clarity.wgsl` uses and for the same reason: it
// keeps `amount == 0` an exact pixel identity even for out-of-gamut pixels,
// where a `companion_encode -> adjust -> companion_decode` round trip would
// clamp. `grain_weight` tapers the delta to zero at the extremes of the
// encoded range, where there is no headroom left to perturb.
//
// `off_x`/`off_y` are the output tile's origin in canvas pixels (see
// `global_vignette.wgsl`'s header for the full note; the GPU backend
// dispatches whole-canvas today so they are zero there, the CPU tiling path
// carries the real offset via `ctx.out_roi`).
//
// Every constant and formula below is duplicated literally from
// `nodes::global::grain`; the CPU/GPU parity test catches drift.
//
// Bind groups follow the §3.2 conventions: input @group(0), write-only
// output storage @group(1), params UBO @group(2).

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    amount: f32,
    inv_cell_fine: f32,
    inv_cell_coarse: f32,
    roughness: f32,
    _pad0: f32,
    off_x: i32,
    off_y: i32,
    _pad1: i32,
    _pad2: i32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: Params;

const GRAIN_RANGE: f32 = 0.30;
const GRAIN_LOW: f32 = 0.05;
const GRAIN_HIGH: f32 = 0.95;
const SEED_FINE: u32 = 0x5f3759dfu;
const SEED_COARSE: u32 = 0x9e3779b9u;

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

fn smoothstep_01(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Zero-taper at both ends of the encoded range: no headroom to perturb at
// clipped black or clipped white.
fn grain_weight(e: f32) -> f32 {
    let lo = smoothstep_01(0.0, GRAIN_LOW, e);
    let hi = 1.0 - smoothstep_01(GRAIN_HIGH, 1.0, e);
    return clamp(lo * hi, 0.0, 1.0);
}

// Bit-exact twin of `nodes::global::grain::hash_u32` (the "lowbias32"
// integer finalizer). WGSL u32 multiplication wraps and `>>` on u32 is a
// logical shift, matching Rust's `wrapping_mul` / `>>`.
fn hash_u32(x: u32) -> u32 {
    var h = x;
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return h;
}

// One lattice sample in [0, 1). The 24-bit mantissa slice keeps the f32
// conversion exact on both backends.
fn lattice_value(ix: i32, iy: i32, seed: u32) -> f32 {
    let a = bitcast<u32>(ix) * 0x9e3779b1u;
    let b = bitcast<u32>(iy) * 0x85ebca6bu;
    let bb = (b << 16u) | (b >> 16u);
    let h = hash_u32(a ^ bb ^ seed);
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

// Smooth (Hermite-interpolated) value noise on the unit lattice, returned
// centred on zero: [-0.5, 0.5). The lerps are written as
// `a * (1 - t) + b * t`, WGSL's own `mix` expansion, so the Rust twin can
// match term for term.
fn value_noise(px: f32, py: f32, seed: u32) -> f32 {
    let fx = floor(px);
    let fy = floor(py);
    let ix = i32(fx);
    let iy = i32(fy);
    let tx = px - fx;
    let ty = py - fy;
    let ux = tx * tx * (3.0 - 2.0 * tx);
    let uy = ty * ty * (3.0 - 2.0 * ty);
    let n00 = lattice_value(ix, iy, seed);
    let n10 = lattice_value(ix + 1, iy, seed);
    let n01 = lattice_value(ix, iy + 1, seed);
    let n11 = lattice_value(ix + 1, iy + 1, seed);
    let a = n00 * (1.0 - ux) + n10 * ux;
    let b = n01 * (1.0 - ux) + n11 * ux;
    return a * (1.0 - uy) + b * uy - 0.5;
}

// The two-octave field in [-1, 1]: fine grain whose amplitude is modulated
// by a coarser field, which is what `roughness` buys.
fn grain_field(ax: f32, ay: f32) -> f32 {
    let n1 = value_noise(ax * params.inv_cell_fine, ay * params.inv_cell_fine, SEED_FINE);
    let n2 = value_noise(ax * params.inv_cell_coarse, ay * params.inv_cell_coarse, SEED_COARSE);
    let r = clamp(params.roughness / 100.0, 0.0, 1.0);
    let amp = 1.0 + r * (2.0 * n2);
    return clamp(2.0 * n1 * amp, -1.0, 1.0);
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let lx = i32(gid.x);
    let ly = i32(gid.y);
    let ax = f32(lx + params.off_x);
    let ay = f32(ly + params.off_y);

    let c = textureLoad(src, vec2<i32>(lx, ly), 0);
    let luma_e = encoded_luma_of(c);
    let n = grain_field(ax, ay);

    // Exactly `0.0` at `amount == 0` for any finite `n`/weight, so the
    // identity contract holds bit-for-bit.
    let delta_encoded = GRAIN_RANGE * (params.amount / 100.0) * n * grain_weight(luma_e);
    let delta_linear = delta_encoded * srgb_eotf_deriv(luma_e);

    var outc = c.rgb + vec3<f32>(delta_linear, delta_linear, delta_linear);
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst, vec2<i32>(lx, ly), vec4<f32>(outc, c.a));
}
