<!--
SPDX-FileCopyrightText: 2026 Lightbox contributors
SPDX-License-Identifier: Apache-2.0
-->

# `lightbox-render` WGSL kernels (E05)

The engine's compute kernels live here, embedded via `include_str!` and
**naga-validated at build time** (spec §3.2 kernel conventions; task **A6**).

Conventions (normative — engine book, task F3):

- one WGSL entry point per pass, workgroup size `16×16×1`;
- `@group(0)` input texture bindings; `@group(1)` write-only output storage
  texture (read-write storage is not portable — ping-pong via `scratch_tiles`);
- `@group(2)` params UBO (std140-compatible, emitted from the `ParamBlock`
  schema); `@group(3)` LUT/aux bindings;
- no `f16` arithmetic (storage is `rgba16float`, compute in f32 — Risk R2);
- no subgroup ops; no runtime shader downloads.

**Scaffold status (A6):** this directory is the frozen home for the kernels; the
build-time naga validation step (a `build.rs` with `naga` as a build-dependency)
and the first kernels are owned by **A-gpu** (task A6). E01's working kernel
`display_transform.wgsl` still lives beside the seed node at
`src/nodes/display_transform.wgsl` until F5 promotes `ng` to the root.
