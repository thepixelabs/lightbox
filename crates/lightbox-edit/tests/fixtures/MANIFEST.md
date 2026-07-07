<!-- SPDX-FileCopyrightText: 2026 Lightbox contributors -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# E09 Phase D — XMP mapping test fixtures (provenance manifest)

Per the E01 fixture-manifest pattern (spec §6). Every file here is
**hand-authored by the Lightbox project** to be representative of the
Lightroom-Classic `crs:` sidecar shape. **None of it is Adobe-authored data**
(the task's honest-reporting rule: "do not invent Adobe data"). The obligation
to test against a real, freely-licensed modern-PV LR Classic corpus is recorded
as **DEFERRED** in `docs/plan/epics/E09-deviations.md` (D-6) for E16/M4.

| file | licence | what it is |
|------|---------|-----------|
| `lr_modern_pv2012.xmp` | CC0 (project-authored) | Representative modern PV2012 (`crs:ProcessVersion=11.0`) sidecar: attribute-form `crs:` scalars, an element-form `ToneCurvePV2012` Seq, a placeholder `MaskGroupBasedCorrections`, and foreign `dc:`/`lr:`/`xmp:` fields for passthrough. |
| `lr_legacy_pv2.xmp` | CC0 (project-authored) | Representative legacy PV2010 (`crs:ProcessVersion=5.7`) sidecar: legacy tone params (`Exposure`/`Brightness`/`Contrast`, no `*2012`) that are skipped-with-report (OQ4). |
| `golden_to_xmp.xmp` | CC0 (project-authored) | Golden full-packet snapshot of `Recipe::to_xmp` for a fixed edited recipe (the authoritative `lb:RecipeCbor` hex is normalized to a length marker so the golden is readable and stable). Regenerate with `LB_BLESS=1 cargo test -p lightbox-edit --test xmp_mapping golden`. |

**Substrate note (D-4).** `MaskGroupBasedCorrections` in a real LR sidecar is an
`rdf:Seq` of `rdf:parseType="Resource"` structs. The Phase-C fallback substrate
models only scalar/simple-array properties, so the fixture uses opaque string
items; E09 classifies the block **skipped** and preserves whatever the substrate
captured. Full verbatim mask preservation is gated on the ISO toolkit (Phase C
reversal) and E12/E16.
