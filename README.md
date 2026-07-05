# Lightbox

An open-source, local-first photo library and raw editor — feature-comparable to Adobe Lightroom, with no subscription and no cloud lock-in.

> **Status**: pre-alpha. Epic E01 (foundation) is implemented and verified: Cargo workspace, crash-proof SQLite catalog, zero-copy egui/wgpu render seam, jobs system, headless core façade, decode probe, first GPU RenderNode with golden-image tests, virtualized-grid shell, and CLI. See [`docs/plan/`](docs/plan/) for the approved v1 plan (16 epics) and [`docs/plan/epics/E01-handoff.md`](docs/plan/epics/E01-handoff.md) for the foundation handoff.

## Building

```sh
cargo xtask fixtures   # fetch the hash-pinned CC0 test corpus (one-time)
cargo build --workspace
cargo test --workspace
cargo run -p lightbox-shell        # the app shell
cargo run -p lightbox-cli -- help  # headless CLI
```

## What Lightbox aims to be

- **A professional DAM**: catalogs that scale to hundreds of thousands of photos, fast culling, collections, smart collections, keywords, EXIF/IPTC metadata, search.
- **A non-destructive raw editor**: full develop pipeline — tone, color, curves, HSL, color grading, detail, optics, geometry — with edit history stored as recipes, never touching originals.
- **AI-assisted, fully local**: subject/sky/people masking, denoise, super-resolution and content-aware removal running on-device via open models. No photos ever leave your machine.
- **Independent**: an original implementation. No Adobe code, assets, or branding — only feature-level compatibility (including reading industry-standard XMP metadata).

## Repository layout

| Path | Contents |
|---|---|
| `docs/research/` | Feature research: what Lightroom does, domain by domain |
| `docs/plan/` | Approved architecture, milestones, and per-epic specs |

## Documentation

- [Project mandate](docs/plan/00-mandate.md) — goals, constraints, non-goals
- Feature catalog, architecture, and implementation plan land in `docs/` as phases complete.
