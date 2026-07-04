# E01 — Implementation deviations from spec

Deviations from `E01-foundation-workspace-catalog.md`, recorded per phase.
Each entry names the spec point, the deviation, and why.

## Phase 1 — Workspace & guardrails

- **Project license selected: Apache-2.0.** Neither the mandate nor the epic
  spec pins the project's own license (mandate constraint 5 requires
  "permissive or weak-copyleft"). Apache-2.0 chosen (ecosystem default,
  patent grant); LICENSE, `LICENSES/`, SPDX headers, and `REUSE.toml` all use
  it. Flag for CTO review if dual MIT/Apache-2.0 is preferred.
- **Crate versions (July 2026) newer than spec-era assumptions:** wgpu 30.0.0
  (`Instance::new` takes `InstanceDescriptor` by value, no `Default` —
  `new_without_display_handle()` used; `request_adapter` returns
  `Result<Adapter, RequestAdapterError>`), thiserror 2.x, serde 1.0.228,
  tracing-subscriber 0.3.23. Spec interfaces unaffected. Edition **2021 kept**
  as the spec/architecture §1.1 explicitly pins it, despite edition 2024
  being current. Toolchain pinned 1.96.1; MSRV recorded as 1.96.
- **xxh3-128 via `twox-hash` (MIT), not `xxhash-rust`.** The commonly-used
  `xxhash-rust` crate is BSL-1.0, which is not on the spec §5 T3 license
  allowlist; `twox-hash` 2.x provides xxh3-128 under MIT. Pins use the
  canonical (big-endian) hex rendering; `lightbox-decode::hash_file`
  (Phase 5) must adopt the same canonicalization.
- **Fixture corpus: 9 CC0 raws instead of "≈12", plus generated non-raws.**
  All seven spec'd mounts are covered (CR2, CR3, NEF ×2, ARW, RAF ×2 — one
  Bayer, one X-Trans — ORF, DNG), ~116 MB total, chosen to keep the CI cache
  small; the manifest schema makes adding more a one-entry change.
  raw.pixls.us hosts raws only, so the JPEG/TIFF/PNG legs and the two corrupt
  fixtures are **deterministically generated** by `cargo xtask fixtures`
  (spec implied downloads for the whole corpus); their manifest entries use a
  `generator` spec instead of a `url`, still hash-pinned and licensed
  (CC0, self-made). The tiny JPEG is a committed 826-byte self-made asset
  (`tools/xtask/assets/`) so its pin can never drift with an encoder upgrade.
- **`docs/plan/licensing.md` and this file created under `docs/`** — the
  epic-execution ground rules say "do not modify docs/", read as no *edits*
  to approved plan documents; both files are new, spec-mandated artifacts
  (T3, and the deviation-recording rule itself).
- **`lightbox-edit`'s frozen `Recipe { schema, pv }` placeholder implemented
  in Phase 1** as part of crate stubbing (spec §2 defines the crate's entire
  E01 content as exactly this placeholder; no later E01 phase claims it, and
  Phase 2's `RenderRequest` needs it).
- **`deny.toml` additions beyond the spec'd allowlist mechanics:**
  `allow-wildcard-paths = true` (intra-workspace path deps carry no version),
  `multiple-versions = "warn"`, yanked-crate denial, and an explicit ban on
  `openssl`. The license allowlist itself is exactly the spec's list.
- **CI toolchain install uses `rustup show active-toolchain`** (rustup
  auto-installs the toolchain pinned in `rust-toolchain.toml`) instead of a
  third-party toolchain action — fewer moving parts, same pin.
- **T4 observability lives in `lightbox-core::observability`** (spec doesn't
  name a crate). File logging is synchronous write-through (daily rotation via
  `tracing-appender`) rather than a buffered background writer, which is what
  makes "panic hook that logs + flushes" trivially true at M0 log volumes.
- **nightly.yml not created in Phase 1.** The workspace layout in spec §2
  lists it, but both of its jobs (long fault-injection, perf harness) are
  Phase 3/8 deliverables (T13, T28); an empty workflow would be noise.
