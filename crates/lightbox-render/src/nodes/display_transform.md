<!--
SPDX-FileCopyrightText: 2026 PixeLabs
SPDX-License-Identifier: AGPL-3.0-or-later
Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
-->

# `display.transform`, algorithm specification

This document is the **written algorithm spec** for the `display.transform`
render node (E01 spec §3.4/§5 T23-T24). The WGSL compute shader
(`display_transform.wgsl`) and the CPU path (`display_transform.rs`,
`eval_cpu`) both implement **exactly this text**, that shared contract is
what makes the CPU/GPU ΔE2000 parity gate (architecture §4.4: max ΔE2000 ≤
1.0 ∧ PSNR ≥ 45 dB) meaningful. Any change to this file is a change to both
implementations and to the committed goldens
(`crates/lightbox-render/goldens/display.transform/`).

The math is deliberately *almost* trivial (E01 spec §3.4): the node's job at
M0 is to prove the `RenderNode` trait, the registry, the ticket lifecycle,
the golden harness and the shared-device composite. E02.3 replaces/extends
the color math (real display ICC); the node's *shape* is the deliverable.

## Contract

* **Input** (exactly one tile at M0 = the whole image): RGBA8 pixels whose
  RGB values are **sRGB-encoded** (the camera's embedded JPEG preview),
  stored **unoriented** (as they sit in the file, orientation is never
  baked into stored pixels, spec §3.1). Alpha is straight (unpremultiplied);
  M0 sources are opaque.
* **Params** (CBOR, spec §3.4): `{ orientation: u8, out_w: u32, out_h: u32 }`
, `orientation` is the EXIF value `1..=8`; anything else is a parameter
  error, never a crash.
* **Output**: RGBA8, sRGB-encoded, `out_w × out_h`, oriented upright.

## Definitions

* `in_w × in_h`, input dimensions (from the input tile).
* **Oriented dimensions**: `(ow, oh) = (in_h, in_w)` when
  `orientation ∈ {5, 6, 7, 8}` (the transposing family), else
  `(in_w, in_h)`.
* **sRGB EOTF (decode)**, per channel `c ∈ [0, 1]`
  (IEC 61966-2-1):
  `lin(c) = c / 12.92` if `c ≤ 0.04045`, else `((c + 0.055) / 1.055)^2.4`.
* **sRGB OETF (encode)**, per channel `c ∈ [0, 1]`:
  `enc(c) = 12.92 · c` if `c ≤ 0.0031308`, else
  `1.055 · c^(1/2.4) − 0.055`.
* **Orientation map** `orient(x, y)`, oriented (upright, integer)
  coordinates → stored coordinates, with `sw = in_w`, `sh = in_h`
  (the standard inverse EXIF transforms; identical to the CPU thumbnail
  bake in `lightbox-preview`):

  | EXIF | stored `(sx, sy)` | meaning |
  |------|-------------------|---------|
  | 1 | `(x, y)` | normal |
  | 2 | `(sw−1−x, y)` | mirror horizontal |
  | 3 | `(sw−1−x, sh−1−y)` | rotate 180° |
  | 4 | `(x, sh−1−y)` | mirror vertical |
  | 5 | `(y, x)` | transpose |
  | 6 | `(y, sh−1−x)` | stored 90° CW of upright |
  | 7 | `(sw−1−y, sh−1−x)` | transverse |
  | 8 | `(sw−1−y, x)` | stored 270° CW of upright |

## Per-output-pixel algorithm

For every output pixel `(x, y)`, `x ∈ [0, out_w)`, `y ∈ [0, out_h)`, in
**f32** arithmetic:

1. **Filter coordinates** (pixel-center convention, integer coordinates
   are pixel centers). Both implementations compute, in this exact
   association:

   ```
   u = (f32(x) + 0.5) * f32(ow) / f32(out_w) - 0.5
   v = (f32(y) + 0.5) * f32(oh) / f32(out_h) - 0.5
   u0 = floor(u);  fu = u - u0
   v0 = floor(v);  fv = v - v0
   ```

2. **Four taps in oriented space**, edges clamped:
   for `(i, j) ∈ {0,1}²`, the tap's oriented integer coordinate is
   `(clamp(u0+i, 0, ow−1), clamp(v0+j, 0, oh−1))`; it is mapped through
   `orient(·)` to stored coordinates and the RGBA8 texel is fetched there.

3. **Decode to linear light**: each tap's R, G, B channel is normalized to
   `[0, 1]` (`c = texel / 255`, i.e. the unorm read) and passed through the
   sRGB EOTF. Alpha is normalized but **not** EOTF-transformed (alpha is
   coverage, not light).

4. **Bilinear blend in linear light**, all four channels, with
   `lerp(a, b, t) = a·(1−t) + b·t` written in exactly that association:

   ```
   c = lerp(lerp(c00, c10, fu), lerp(c01, c11, fu), fv)
   ```

5. **Encode**: R, G, B through the sRGB OETF; alpha passes through.
   Clamp each channel to `[0, 1]` and quantize to 8 bits.

## Determinism & tolerance notes

* The CPU path (`rayon` over output rows) computes each pixel as a pure
  function of the inputs, **byte-stable across runs and thread counts** on
  the same machine (asserted by test; the `lightbox-cli render --cpu`
  byte-stability AC rides on this).
* CPU quantization is round-half-up (`floor(c·255 + 0.5)`); GPU
  quantization is the backend's float→unorm8 conversion (round-to-nearest,
  tie behavior implementation-defined). `pow` may differ by a few ULP
  between CPU libm and GPU hardware. Both effects are bounded by ±1 LSB
  far inside the §4.4 perceptual gate, which is why goldens compare with
  ΔE2000/PSNR rather than byte equality.
* Goldens are **blessed from the CPU path** (the reference); the GPU path
  is verified against those same goldens and against the CPU output.

## Explicit non-goals at M0 (owning epics)

* Real color management (camera matrices, DCP, display ICC), **E02**.
  M0 assumes an sRGB display, stated honestly in the UI.
* Tiling/ROI (the tile types carry `(offset, extent)` so E05.3 changes tile
  *production*, not this node), caching, progressive refinement, **E05**.
* Resampling quality beyond bilinear (Lanczos/Mitchell for exports)
  **E10/E15**.
