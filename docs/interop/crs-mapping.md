<!-- SPDX-FileCopyrightText: 2026 PixeLabs -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms. -->

# Lightbox ↔ Adobe Camera Raw (`crs:`) develop-settings mapping

**Normative.** This document is the human-readable projection of the in-code
`FIELD_TABLE` in `crates/lightbox-edit/src/xmp_map/convert.rs`; the two must not
drift (the converter unit tests are generated from the table, T18). It defines,
for every §3.2 recipe field, the corresponding Adobe Camera Raw / Lightroom
`crs:` property, the value domains, the conversion, and a **fidelity class**.
E16 extends this table for the M4 honor-LR-sidecar-on-open flow; it never forks
it.

> **Review status (T28 / DoD §9.7).** Walked and confirmed at the E09 Phase-E
> gate against the shipped `FIELD_TABLE`, `to_xmp`/`from_lr_crs`, and the Phase-E
> preset-import present-key subset detection (which consumes the same field paths):
> every §3.2 global/geometry field is classified, and the skipped-by-design set
> (masks/retouch, PV≤2 tone, administrative flags) is complete. The formal
> cross-epic sign-off named in DoD §9.7 (**E16 owner + one pixel-stream engineer**)
> is **DEFERRED**, those roles are not present in this execution context; the
> document is stable and ready for that sign-off (recorded in `E09-deviations.md`).

## Scope and the two directions

Lightbox owns this mapping **regardless of the XMP substrate** (architecture
§1.6). It backs three operations (`lightbox_edit::xmp_map`):

- **`Recipe::to_xmp` (emit).** Writes the authoritative `lb:` payload (full
  fidelity, below) **plus** a best-effort `crs:` compatibility projection so
  Lightroom/ACR readers see recognizable develop settings, **plus** any foreign
  fields re-emitted verbatim. Only **non-neutral** `crs:` fields are written; an
  absent `crs:` property reads back as its default in any ACR-family app.
- **`Recipe::read_xmp` (read).** Prefers the `lb:` payload (a **struct-equal**
  reconstruction). A foreign, `lb:`-less doc routes to `from_lr_crs`.
- **`Recipe::from_lr_crs` (import).** The `crs:` read mechanism (Risk 9). It is
  **total**, never panics or errors on malformed input, and produces a
  `CrsImportReport` whose `mapped ∪ approximate ∪ skipped` partition covers
  **every `crs:` key seen**. Unmapped `crs:` keys and every foreign namespace are
  preserved verbatim in `Recipe.xmp_passthrough`.

### Authoritative round-trip is `lb:`, not `crs:`

The `crs:` projection is **lossy by construction** (differing tone/colour models,
integer-quantized curves, enum remaps). Lightbox's own sidecars therefore carry
an `lb:` block that is the source of truth:

| `lb:` property | Meaning |
|---|---|
| `lb:RecipeCbor` | Hex of the canonical CBOR recipe, the full-fidelity payload `read_xmp` reconstructs from. |
| `lb:Schema` | `RECIPE_SCHEMA` at write time. |
| `lb:ProcessVersion` | The recipe's process version. |
| `lb:CreatorTool` | Writing application (provenance). |

A reader that understands `lb:` gets the exact recipe back; a reader that only
understands `crs:` gets a faithful-as-possible approximation. **Do not read the
`crs:` values back into Lightbox when `lb:` is present**, `lb:` wins.

## Fidelity classes

- **Exact**, 1:1 numeric or enumerant mapping, no domain loss.
- **Approximate**, domain-translated: curve quantization, split-toning→grade,
  enum remap, or a differing WB/vignette/transform model.
- **Skipped**, not mapped through `crs:` by design (a Lightbox-only field, or
  content owned by another epic). Full fidelity survives via `lb:` only; on
  import, a skipped `crs:` key is preserved verbatim in `xmp_passthrough`.

## Process-version classification

`crs:ProcessVersion` (LR's numeric version) gates tone-parameter interpretation:

| `crs:ProcessVersion` | Class | Behaviour |
|---|---|---|
| ≥ 6.7 (e.g. `6.7`, `11.0`, `15.0`) | **Modern** (PV2012 family) | `*2012` tone params map. |
| < 6.7 (e.g. `5.0` = PV2003, `5.7` = PV2010) | **Legacy** | Legacy tone params (`Exposure`/`Brightness`/`Contrast`, no `*2012`) are **skipped-with-report** (OQ4; E16 revisits at M4 with corpus evidence, Adobe's 2010→2012 conversion is unpublished). |
| absent / unparseable | **Unknown** | Treated as modern (safe default); `*2012` reads attempted. |

## The field table (every §3.2 global + geometry field)

| Lightbox recipe field | `crs:` property | Domain | Conversion | Fidelity | Notes |
|---|---|---|---|---|---|
| `base_profile` | `CameraProfile` | profile id ↔ name | name hint (identity not resolvable by LR) | Approximate | Lightbox profile identity emitted as a hint; full fidelity via `lb:`. |
| `global.white_balance` | `WhiteBalance` (+ `Temperature`/`Tint`) | mode + temp/tint | `wb_to_crs` / `wb_from_crs` | Approximate | `Custom` temp is CCT (K); LR non-raw uses relative temp. |
| `global.exposure` | `Exposure2012` | stops −5..5 | identity (stops) | Exact | |
| `global.contrast` | `Contrast2012` | −100..100 | identity | Exact | |
| `global.highlights` | `Highlights2012` | −100..100 | identity | Exact | |
| `global.shadows` | `Shadows2012` | −100..100 | identity | Exact | |
| `global.whites` | `Whites2012` | −100..100 | identity | Exact | |
| `global.blacks` | `Blacks2012` | −100..100 | identity | Exact | |
| `global.tone_curve.rgb` | `ToneCurvePV2012` | `[0,1]²` ↔ 0..255 `Seq` | `curve_to_crs` / `curve_from_crs` | Approximate | Quantized to the 0-255 integer grid. |
| `global.tone_curve.r` | `ToneCurvePV2012Red` | `[0,1]²` ↔ 0..255 `Seq` | `curve_to_crs` | Approximate | |
| `global.tone_curve.g` | `ToneCurvePV2012Green` | `[0,1]²` ↔ 0..255 `Seq` | `curve_to_crs` | Approximate | |
| `global.tone_curve.b` | `ToneCurvePV2012Blue` | `[0,1]²` ↔ 0..255 `Seq` | `curve_to_crs` | Approximate | |
| `global.hsl` | `HueAdjustment*` / `SaturationAdjustment*` / `LuminanceAdjustment*` | 8 bands × 3, −100..100 | identity per band | Exact | Bands: Red, Orange, Yellow, Green, Aqua, Blue, Purple, Magenta. |
| `global.color_grade` | `ColorGrade{Shadow,Midtone,Highlight,Global}{Hue,Sat,Lum}` + `ColorGradeBlending` + `ColorGradeBalance` | hue 0..360, sat/lum, blend, balance | identity per wheel; `split_toning_to_color_grade` on legacy read | Approximate | Legacy `SplitToning*` is approximated into the shadow/highlight wheels; modern `ColorGrade*` overrides it. |
| `global.treatment` | `ConvertToGrayscale` | Color \| B&W ↔ bool | treatment ↔ bool | Exact | |
| `global.bw` | `GrayMixer{Red…Magenta}` | 8 × −100..100 | identity per channel | Exact | |
| `global.vibrance` | `Vibrance` | −100..100 | identity | Exact | |
| `global.saturation` | `Saturation` | −100..100 | identity | Exact | PV-independent; maps even on a legacy doc. |
| `global.presence.clarity` | `Clarity2012` | −100..100 | identity | Exact | |
| `global.presence.texture` | `Texture` | −100..100 | identity | Exact | |
| `global.presence.dehaze` | `Dehaze` | −100..100 | identity | Exact | |
| `global.detail.sharpen.amount` | `Sharpness` | 0..150 | identity | Exact | |
| `global.detail.sharpen.radius` | `SharpenRadius` | 0.5..3.0 | identity | Exact | |
| `global.detail.sharpen.detail` | `SharpenDetail` | 0..100 | identity | Exact | |
| `global.detail.sharpen.masking` | `SharpenEdgeMasking` | 0..100 | identity | Exact | |
| `global.detail.nr.luma` | `LuminanceSmoothing` | 0..100 | identity | Exact | |
| `global.detail.nr.luma_detail` | `LuminanceNoiseReductionDetail` | 0..100 | identity | Exact | |
| `global.detail.nr.chroma` | `ColorNoiseReduction` | 0..100 | identity | Exact | |
| `global.detail.nr.chroma_detail` | `ColorNoiseReductionDetail` | 0..100 | identity | Exact | |
| `global.optics.lens_profile` | `LensProfileEnable` / `LensProfileName` / `LensProfileDistortionScale` / `LensProfileVignettingScale` | profile + scales | enable + scales; identity is Lightbox's | Approximate | Profile identity is Lightbox's; not resolvable by LR. |
| `global.optics.ca` | `AutoLateralCA` | bool | bool | Exact | |
| `global.optics.defringe` | `DefringePurpleAmount` | 0..100 (single) ↔ purple/green | single → purple | Approximate | LR splits purple/green; Lightbox has one amount at M1 (deviations A-4). |
| `global.optics.vignette_corr` | `VignetteAmount` | −100..100 | identity | Approximate | Manual lens-vignette model differs slightly. |
| `global.effects.postcrop_vignette` | `PostCropVignetteAmount` (+ `Midpoint`/`Feather`/`Roundness`/`HighlightContrast`) | amount + style | amount identity; style enum | Approximate | Style enum not fully modeled at M1. |
| `global.effects.grain` | `GrainAmount` (+ `GrainSize` / `GrainFrequency`) | amount/size/frequency | amount identity; roughness ↔ frequency | Approximate | `roughness ↔ GrainFrequency` is approximate. |
| `global.effects.creative_lut` |, | Lightbox LUT ref |, | **Skipped** | No `crs:` peer; `lb:` full-fidelity only. |
| `geometry.crop` | `CropTop`/`Left`/`Bottom`/`Right` + `HasCrop` | `[0,1]` edges | identity (normalized) | Exact | |
| `geometry.angle` | `CropAngle` | −45..45 deg | identity (sign per LR) | Approximate | LR `CropAngle` sign/convention differs. |
| `geometry.flip` |, | None \| H \| V \| Both |, | **Skipped** | LR encodes flip via crop-orientation flags; `lb:` only at M1. |
| `geometry.upright` | `PerspectiveUpright` | Off/Auto/Level/Vertical/Full ↔ 0..5 | `upright_to_crs` / `upright_from_crs` | Approximate | Enum remap; LR `5 = Guided` has no Lightbox peer. |
| `geometry.transform` | `Perspective{Vertical,Horizontal,Rotate,Aspect,Scale,X,Y}` | 7 × −100..100 | identity per axis | Approximate | Manual-transform model differs. |

## Skipped-by-design (structure & legacy)

These are **never** translated through `crs:`. On import they are classified
`skipped` and preserved verbatim in `xmp_passthrough`; on export they are not
synthesized from the recipe (their content, if any, lives in `lb:` / other
epics).

| `crs:` property | Why skipped | Owner |
|---|---|---|
| `MaskGroupBasedCorrections` | `Recipe.masks` is **ids-only** (§3.1 ownership rule); mask content and any `crs:`→mask translation are out of E09. | E12 / E16 |
| `RetouchAreas` | `Recipe.retouch` is **ids-only**. | E12 / E14 / E16 |
| `Exposure`, `Brightness`, `Contrast` (legacy, no `*2012`) | Pre-2012 (PV≤2) tone params; Adobe's 2010→2012 conversion is unpublished (OQ4). Skipped-with-report; E16 decides best-effort translation at M4. | E16 |
| `Version`, `ProcessVersion`, `HasSettings`, `HasCrop`, and any unrecognized `crs:` key | Administrative / provenance flags, not develop parameters. Preserved verbatim so a round-trip keeps the source's own head. |, |

## Known interop limitations (stated, per Risk R2/R3)

- **Reading `crs:` is not rendering identically.** The fidelity classes above
  are honest about where a value is approximated. The `CrsImportReport` surfaces
  this per field; E08/E16 present it to the user. Lightbox never *silently*
  applies a foreign sidecar (the honor-on-open flow is E16/M4).
- **Sidecar-only writes.** Lightbox writes `.xmp` sidecars only and never embeds
  XMP into an original (mandate c.2). Tools that read only embedded XMP in
  JPEG/DNG will not see Lightbox edits until export (E15).
- **Nested-struct properties.** With the Phase-C fallback substrate, nested
  `rdf:parseType="Resource"` structures (e.g. real LR mask groups) are not
  modeled and cannot be preserved verbatim; full struct fidelity is the ISO
  toolkit's job (deviations D-4).
