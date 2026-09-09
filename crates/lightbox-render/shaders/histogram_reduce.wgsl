// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// histogram_reduce.wgsl, E10 Phase D task D12 `HistogramPass`'s GPU
// reduction. Bins the RGBA8 display-space `src` texture (the canvas's
// published `rgba8unorm` frame, spec `CANVAS_FORMAT`) into per-channel
// 256-bin histograms (R/G/B/luma) plus highlight/shadow clip counts, all via
// `atomicAdd` into a shared storage buffer, the standard GPU-histogram
// technique (one atomic increment per pixel per plane, no local/workgroup
// pre-reduction needed at preview resolution).
//
// Deviation from the `shaders/README.md` §3.2 convention ("`@group(1)`
// write-only output storage TEXTURE"): this pass's output is not an image
// it reduces to scalar counts, so `@group(1)` binds two READ-WRITE storage
// BUFFERS (`bins`, `clip`) instead. Storage-buffer atomics are core WGSL (no
// feature gate, unlike read-write storage *textures*), so this stays fully
// portable; recorded in `docs/plan/epics/E10-deviations.md` Phase D.
//
// Bin layout (`bins`, length 1024 = 4 * 256): `[0..256)` = R, `[256..512)` =
// G, `[512..768)` = B, `[768..1024)` = luma. `clip` (length 2): `[0]` =
// highlight-clipped pixel count (any of r/g/b == 255), `[1]` = shadow-clipped
// (any of r/g/b == 0). Luma uses a fixed-point Rec. 709 approximation
// (`54/183/19` over 256, summing to exactly 256) so the bin index is
// **exact integer arithmetic**, bit-identical between this kernel and the
// CPU reference (`nodes::global::histogram::luma_bin`), the D12 "bins match a
// CPU reference EXACTLY" acceptance criterion.
//
// `src`'s r/g/b are recovered from the unorm-decoded float via
// `round(clamp(c, 0, 1) * 255)`: the unorm8 <-> normalized-float conversion
// is a lossless round trip by construction (WebGPU/wgpu spec), so this
// recovers the exact original 8-bit channel value on real hardware, proven
// by this pass's own GPU-vs-CPU exact-match test
// (`tests/e10_histogram.rs`), not merely assumed.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var<storage, read_write> bins: array<atomic<u32>>;
@group(1) @binding(1) var<storage, read_write> clip: array<atomic<u32>>;

struct Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }
    let c = textureLoad(src, vec2<i32>(gid.xy), 0);
    let r8 = u32(round(clamp(c.r, 0.0, 1.0) * 255.0));
    let g8 = u32(round(clamp(c.g, 0.0, 1.0) * 255.0));
    let b8 = u32(round(clamp(c.b, 0.0, 1.0) * 255.0));

    atomicAdd(&bins[r8], 1u);
    atomicAdd(&bins[256u + g8], 1u);
    atomicAdd(&bins[512u + b8], 1u);

    // Rec. 709 luma, fixed-point (weights sum to exactly 256, see module doc).
    let luma = (r8 * 54u + g8 * 183u + b8 * 19u) >> 8u;
    atomicAdd(&bins[768u + luma], 1u);

    if r8 == 255u || g8 == 255u || b8 == 255u {
        atomicAdd(&clip[0], 1u);
    }
    if r8 == 0u || g8 == 0u || b8 == 0u {
        atomicAdd(&clip[1], 1u);
    }
}
