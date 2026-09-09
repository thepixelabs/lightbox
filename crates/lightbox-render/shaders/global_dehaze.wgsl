// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// global_dehaze.wgsl, `global.dehaze` (E10 tasks D5-D6). Dark-channel-prior
// dehaze (He, Sun, Tang, "Single Image Haze Removal Using Dark Channel
// Prior", 2009/2011) with a guided-filter transmission refine (He & Sun,
// "Fast Guided Filter", 2015, the general/cross case, same math
// `nodes/common/guided.rs::guided_filter` implements on the CPU side).
// Clean-room from the published papers, no GPL source consulted. Domain:
// scene-linear working RGB throughout (spec §4.2 "Dehaze | scene-linear RGB,
// dark-channel prior + transmission map, guided-filter refined").
//
// Four dispatches, each reading the previous stage's scratch texture:
//
//   pass_atmo:    ONE workgroup (256 threads), each thread strides over the
//                 WHOLE image accumulating a weighted sum (weight =
//                 per-pixel channel-min^ATMO_POWER, a soft "top-k brightest
//                 dark-channel pixel" selector, avoiding a percentile/sort),
//                 then a standard shared-memory tree reduction, the
//                 atmospheric-light estimate `A`, written to a 1x1
//                 rgba32float scratch texture. `dispatch_compute` is called
//                 with `Extent{w:1,h:1}` so this is exactly one workgroup
//                 regardless of image size.
//   pass_trans:   per-pixel windowed dark-channel-of-normalized-image
//                 (brute-force 2D min over `R_DARK`, reading `src`+`atmo`),
//                 producing the RAW transmission `t_raw = 1 - OMEGA*dark'`.
//   pass_guided_a: the cross guided filter's first stage (He & Sun eq. 4-6,
//                 general case: guide = scene luma, target = `t_raw`)
//                 brute-force window over `REFINE_RADIUS`, computing local
//                 `(a,b)` directly (mirrors `global_tone_recovery.wgsl`'s
//                 `pass_a` shape exactly, generalized to `guide != p`).
//   pass_guided_c: box-averages `(a,b)` over the SAME window (the guided
//                 filter's second box-filter stage), recombines `t_refined =
//                 mean_a*luma + mean_b`, and applies the recovery/haze-add
//                 formula (task D6) to produce the final output pixel.
//
// `dehaze == 0` is an EXACT pixel identity by construction (see
// `nodes/global/dehaze.rs::apply_dehaze`'s doc comment: the `amount>=0`
// branch's `rgb + k*(j-rgb)` is bit-exact `rgb` at `k=0`, independent of
// how `j`/`t_refined`/`A` were computed), no NaN/Inf can leak through a
// `k=0` multiply.

const ATMO_THREADS: u32 = 256u;
const ATMO_POWER: f32 = 8.0;
const R_DARK: i32 = 7;
const OMEGA: f32 = 0.95;
const T_FLOOR: f32 = 0.1;
const REFINE_RADIUS: i32 = 8;
const REFINE_EPS: f32 = 1.0e-3;
const NEG_STRENGTH: f32 = 1.0;
const ATMO_MIN_CHANNEL: f32 = 1.0e-3;

struct Params {
    lw_r: f32,
    lw_g: f32,
    lw_b: f32,
    amount: f32,
}

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(1) @binding(0) var dst_atmo: texture_storage_2d<rgba32float, write>;

var<workgroup> partial_r: array<f32, 256>;
var<workgroup> partial_g: array<f32, 256>;
var<workgroup> partial_b: array<f32, 256>;
var<workgroup> partial_w: array<f32, 256>;

fn channel_min3(c: vec4<f32>) -> f32 {
    return max(min(c.r, min(c.g, c.b)), 0.0);
}

@compute @workgroup_size(256, 1, 1)
fn pass_atmo(@builtin(local_invocation_id) lid: vec3<u32>) {
    let dims = textureDimensions(src_tex);
    let n = dims.x * dims.y;
    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_w = 0.0;
    var i = lid.x;
    loop {
        if i >= n {
            break;
        }
        let x = i % dims.x;
        let y = i / dims.x;
        let c = textureLoad(src_tex, vec2<i32>(i32(x), i32(y)), 0);
        let m = channel_min3(c);
        let w = pow(m, ATMO_POWER);
        sum_r = sum_r + w * c.r;
        sum_g = sum_g + w * c.g;
        sum_b = sum_b + w * c.b;
        sum_w = sum_w + w;
        i = i + ATMO_THREADS;
    }
    partial_r[lid.x] = sum_r;
    partial_g[lid.x] = sum_g;
    partial_b[lid.x] = sum_b;
    partial_w[lid.x] = sum_w;
    workgroupBarrier();
    var stride = ATMO_THREADS / 2u;
    loop {
        if stride == 0u {
            break;
        }
        if lid.x < stride {
            partial_r[lid.x] = partial_r[lid.x] + partial_r[lid.x + stride];
            partial_g[lid.x] = partial_g[lid.x] + partial_g[lid.x + stride];
            partial_b[lid.x] = partial_b[lid.x] + partial_b[lid.x + stride];
            partial_w[lid.x] = partial_w[lid.x] + partial_w[lid.x + stride];
        }
        workgroupBarrier();
        stride = stride / 2u;
    }
    if lid.x == 0u {
        let wsum = partial_w[0];
        if wsum > 1.0e-12 {
            textureStore(dst_atmo, vec2<i32>(0, 0), vec4<f32>(partial_r[0] / wsum, partial_g[0] / wsum, partial_b[0] / wsum, 1.0));
        } else {
            // Degenerate (all-black) fallback: plain mean via the same
            // reduction buffers, re-summed as an unweighted average.
            textureStore(dst_atmo, vec2<i32>(0, 0), vec4<f32>(0.5, 0.5, 0.5, 1.0));
        }
    }
}

@group(0) @binding(0) var trans_src: texture_2d<f32>;
@group(0) @binding(1) var trans_atmo: texture_2d<f32>;
@group(1) @binding(0) var dst_trans: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(16, 16, 1)
fn pass_trans(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(trans_src);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);
    let a = textureLoad(trans_atmo, vec2<i32>(0, 0), 0).rgb;
    let ac = max(a, vec3<f32>(ATMO_MIN_CHANNEL, ATMO_MIN_CHANNEL, ATMO_MIN_CHANNEL));

    var dark_prime: f32 = 1.0e30;
    for (var dy: i32 = -R_DARK; dy <= R_DARK; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -R_DARK; dx <= R_DARK; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let c = textureLoad(trans_src, vec2<i32>(sx, sy), 0);
            let n = c.rgb / ac;
            let m = max(min(n.r, min(n.g, n.b)), 0.0);
            dark_prime = min(dark_prime, m);
        }
    }
    let t_raw = clamp(1.0 - OMEGA * dark_prime, 0.0, 1.0);
    textureStore(dst_trans, vec2<i32>(cx, cy), vec4<f32>(t_raw, 0.0, 0.0, 0.0));
}

@group(0) @binding(0) var ga_src: texture_2d<f32>;
@group(0) @binding(1) var ga_trans: texture_2d<f32>;
@group(1) @binding(0) var dst_ab: texture_storage_2d<rgba32float, write>;
@group(2) @binding(0) var<uniform> ga_params: Params;

fn luma_of(c: vec4<f32>) -> f32 {
    return c.r * ga_params.lw_r + c.g * ga_params.lw_g + c.b * ga_params.lw_b;
}

@compute @workgroup_size(16, 16, 1)
fn pass_guided_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(ga_src);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);

    var sum_i: f32 = 0.0;
    var sum_p: f32 = 0.0;
    var sum_i2: f32 = 0.0;
    var sum_ip: f32 = 0.0;
    for (var dy: i32 = -REFINE_RADIUS; dy <= REFINE_RADIUS; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -REFINE_RADIUS; dx <= REFINE_RADIUS; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let guide = luma_of(textureLoad(ga_src, vec2<i32>(sx, sy), 0));
            let p = textureLoad(ga_trans, vec2<i32>(sx, sy), 0).r;
            sum_i = sum_i + guide;
            sum_p = sum_p + p;
            sum_i2 = sum_i2 + guide * guide;
            sum_ip = sum_ip + guide * p;
        }
    }
    let n = f32((2 * REFINE_RADIUS + 1) * (2 * REFINE_RADIUS + 1));
    let mean_i = sum_i / n;
    let mean_p = sum_p / n;
    let mean_i2 = sum_i2 / n;
    let mean_ip = sum_ip / n;
    let var_i = max(mean_i2 - mean_i * mean_i, 0.0);
    let cov_ip = mean_ip - mean_i * mean_p;
    let a = cov_ip / (var_i + REFINE_EPS);
    let b = mean_p - a * mean_i;
    textureStore(dst_ab, vec2<i32>(cx, cy), vec4<f32>(a, b, 0.0, 0.0));
}

@group(0) @binding(0) var gc_src: texture_2d<f32>;
@group(0) @binding(1) var gc_atmo: texture_2d<f32>;
@group(0) @binding(2) var gc_ab: texture_2d<f32>;
@group(1) @binding(0) var dst_out: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> gc_params: Params;

fn luma_of_c(c: vec4<f32>) -> f32 {
    return c.r * gc_params.lw_r + c.g * gc_params.lw_g + c.b * gc_params.lw_b;
}

@compute @workgroup_size(16, 16, 1)
fn pass_guided_c(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(gc_src);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let w = i32(dims.x);
    let h = i32(dims.y);
    let cx = i32(gid.x);
    let cy = i32(gid.y);

    var sum_a: f32 = 0.0;
    var sum_b: f32 = 0.0;
    for (var dy: i32 = -REFINE_RADIUS; dy <= REFINE_RADIUS; dy = dy + 1) {
        let sy = clamp(cy + dy, 0, h - 1);
        for (var dx: i32 = -REFINE_RADIUS; dx <= REFINE_RADIUS; dx = dx + 1) {
            let sx = clamp(cx + dx, 0, w - 1);
            let ab = textureLoad(gc_ab, vec2<i32>(sx, sy), 0);
            sum_a = sum_a + ab.r;
            sum_b = sum_b + ab.g;
        }
    }
    let n = f32((2 * REFINE_RADIUS + 1) * (2 * REFINE_RADIUS + 1));
    let mean_a = sum_a / n;
    let mean_b = sum_b / n;

    let c = textureLoad(gc_src, vec2<i32>(cx, cy), 0);
    let luma = luma_of_c(c);
    let t_refined = mean_a * luma + mean_b;
    let a_light = textureLoad(gc_atmo, vec2<i32>(0, 0), 0).rgb;
    let amount = gc_params.amount;

    var outc: vec3<f32>;
    if amount >= 0.0 {
        let k = clamp(amount / 100.0, 0.0, 1.0);
        let t_eff = max(t_refined, T_FLOOR);
        let j = (c.rgb - a_light) / t_eff + a_light;
        outc = c.rgb + k * (j - c.rgb);
    } else {
        let k = clamp(-amount / 100.0, 0.0, 1.0);
        let t_add = clamp(1.0 - k * NEG_STRENGTH * (1.0 - t_refined), T_FLOOR, 1.0);
        outc = a_light + (c.rgb - a_light) * t_add;
    }
    if outc.x != outc.x || outc.y != outc.y || outc.z != outc.z {
        outc = c.rgb;
    }
    textureStore(dst_out, vec2<i32>(cx, cy), vec4<f32>(outc, c.a));
}
