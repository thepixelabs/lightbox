# Lightbox development notes

Editing-first, local-first, open-source **raw photo editor** in Rust (egui + wgpu node-graph engine + SQLite edit store + local ONNX AI). **There is no library/DAM**, drag-and-drop / OS open dialog → editor with filmstrip. That cut is an owner decision; never reintroduce library features.

## Getting oriented

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) first. It is a map of the workspace
and the handful of decisions everything else is built on: the node graph, the
edit store, the out-of-process raw decoder, and why each is shaped the way it
is. Then read the crate you are about to touch. The crates carry module level
doc comments that explain intent rather than restating the code, so
`cargo doc --open` is a reasonable second stop.

[`docs/engine-book/`](docs/engine-book/) is the guide to the render engine
specifically, including how to add a node. [`docs/interop/crs-mapping.md`](docs/interop/crs-mapping.md)
documents how recipe fields map onto Adobe's `crs:` XMP namespace, which is
what makes sidecars readable by other software.

## Build & verify

```sh
export PATH="$HOME/.cargo/bin:$PATH"    # if cargo is not already on your PATH
cargo xtask fixtures                     # one-time fixture fetch
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
cargo deny check                         # license gate (GPL never in-process)
```

Exit bar for ANY commit: all five green. Never commit broken. If you depart from what a doc comment says, update the doc comment in the same commit.

## Guardrails (owner intent)

- Color/raw editing is the product; complete raw parameter surface on raw files; same UI minus raw stages otherwise.
- AI runs offline only. AI Looks proposals must be **editable recipes** (grading wheels/curves/HSL deltas), never baked filters; shuffle is seeded-deterministic.
- `cargo run -p lightbox-shell -- --smoke 60` exits after ~1 s **by design** (self-test, not a crash).
- Report honestly: failed gates are failures; skipped steps get named.
