<!-- SPDX-FileCopyrightText: 2026 PixeLabs -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms. -->

# Target-shot capture protocol (E02.5 / Phase G, task G2)

The machine-readable half of this protocol is the `session.toml` schema in
[`src/session.rs`](../src/session.rs). This document is the human procedure:
how to shoot a color-target session that [`lightbox-profgen`](../) can turn into
a validated curated `.dcp`.

> **Status: DRAFT, content-line owner review pending.** This protocol has **not**
> been reviewed by a named content-line owner because the content line is
> **unstaffed** (Open Question #2 / Risk 4 / architecture §12). The unstaffed
> state is escalated to the CTO/operator in [`runbook.md`](./runbook.md) §Escalation.
> No physical capture sessions have been run and **zero** profiles ship (spec §0).

---

## 1. What a session produces

One **session** = one camera body, shot against one reference chart, under **two**
illuminants (Standard A + a daylight), yielding the dual-illuminant calibration
data a DNG-style profile interpolates between (`ColorMatrix1/2`,
`ForwardMatrix1/2`, `CalibrationIlluminant1/2`). The output is a single
`<make>/<model>.dcp` plus its provenance.

## 2. Equipment

| Item | Requirement |
|------|-------------|
| Chart | X-Rite/Calibrite **ColorChecker Classic (24)** minimum; **Digital SG (140)** preferred for LUT-quality profiles; IT8 acceptable for transmissive work. One physical chart per session, its batch/serial recorded. |
| Standard A source | Tungsten ~2856 K (a stabilized 3200 K halogen gelled/measured to A, or a calibrated A source). |
| Daylight source | D50-D65: a daylight-simulator booth, or measured open-shade/midday sun. Record the measured CCT. |
| Meter | A color meter or spectrometer to record the actual illuminant CCT/tint (do **not** assume nominal). |
| Mount | Copy stand or tripod; chart flat and square to the sensor. |

## 3. Capture procedure (per illuminant)

1. **Mount flat & square.** Chart fills 30-60 % of frame, normal to the axis;
   no keystoning.
2. **Even, flat-field lighting.** Two lights at ~45°, equal power, far enough to
   avoid falloff. Corner-to-center luminance variation on a gray card **≤ 2 %**.
   Kill all specular glare (cross-polarize if needed).
3. **Native white balance.** Shoot **raw**. WB in-camera is irrelevant (we read
   the raw), but note the as-shot neutral.
4. **Base ISO, fixed aperture** in the lens's sharp/even range (~f/5.6-f/8).
5. **Expose the light-gray patch to ~⅔-¾ full scale**, the brightest neutral
   **not clipped in any channel**. Verify on the raw histogram, not the JPEG.
   Highlight clipping on a patch **invalidates** the shot.
6. **Bracket ±1 stop** around that exposure; keep the best-exposed unclipped frame.
7. Repeat the entire procedure under the **second** illuminant without moving the
   chart or camera if possible.

## 4. Flat-field / sanity rules (hard gates)

- No patch clipped (raw max) in any channel.
- No patch crushed to black-level in any channel.
- Neutral row reads neutral to the eye (no obvious cast from mixed lighting).
- Single light source per shot, **no mixed color temperatures** in one frame.
- Chart clean, unfaded, within its manufacturer age/fade window.

A frame failing any of these is discarded, not "corrected".

## 5. Session directory + metadata

Lay the raws out in one directory with a `session.toml` (schema:
[`SessionMeta`](../src/session.rs)):

```toml
session_id  = "session-2026-07-06-nikon-z6"
captured_at = "2026-07-06"
operator    = "<name>"
chart       = "color-checker24"          # color-checker24 | color-checker-sg | it8

[body]
make  = "NIKON CORPORATION"              # EXIF Make, as-is (normalized on read)
model = "NIKON Z 6"                      # EXIF Model, as-is → (Nikon, Z6)

[[illuminants]]
name = "StdA"
cct  = 2856.0

[[illuminants]]
name = "daylight"
cct  = 6504.0                            # the MEASURED CCT, not the nominal

[[raws]]
illuminant = "StdA"
path       = "stda.nef"

[[raws]]
illuminant = "daylight"
path       = "d65.nef"
```

`lightbox-profgen generate <session-dir>` normalizes `body` to a `CameraId`,
schema-checks the session, and (with `--features dcamprof` + the tool installed)
runs the two-stage `dcamprof` pipeline. See [`runbook.md`](./runbook.md).

## 6. Retention & provenance

- **Keep the source raws.** They are the reproducibility record and the input to
  any future re-profiling. Retention policy: [`runbook.md`](./runbook.md) §Retention.
- The generated profile's provenance is `profgen:<session_id>` plus the dcamprof
  version, written to `assets/MANIFEST.toml` by the packaging stage (G4). Every
  profile is traceable to its session.
- **No third-party / Adobe-authored data** at any step. The manifest checker
  fails the build on any Adobe authorship marker (constraint 4, spec §4.3).

## 7. Printable pre-flight checklist

```
[ ] Body + lens recorded; base ISO; f/5.6–f/8
[ ] Chart flat, square, clean, in-date; batch/serial noted
[ ] Illuminant #1 = Standard A (~2856 K); CCT/tint metered & recorded
[ ] Illuminant #2 = daylight; measured CCT/tint recorded
[ ] Flat field verified (gray card ≤ 2% corner-to-center)
[ ] Glare killed (cross-polarized if needed)
[ ] Light-gray patch ~⅔–¾ scale, NO channel clipped (checked on raw)
[ ] ±1 stop bracket shot under each illuminant
[ ] Raws saved to session dir; session.toml filled; source raws archived
```
