# Lightbox development notes

Editing-first, local-first, open-source **raw photo editor** in Rust (egui + wgpu node-graph engine + SQLite edit store + local ONNX AI). **There is no library/DAM**, drag-and-drop / OS open dialog → editor with filmstrip. That cut is an owner decision; never reintroduce library features.

## Getting oriented (start here)

1. Read `docs/plan/03-handoff.md`, the single source of truth for project state: what's built, what's specced, the epic board with dependencies, and the development process. Do NOT try to read the whole `docs/` tree.
2. Cross-check with `git log --oneline -15`: commits named `E<nn> Phase <n>: …` show implementation progress; the epic board in the handoff tells you what's next.
3. Work the next item per the handoff's **§5 development process** (phase-by-phase implementation with a green-build gate on every commit; scope changes go through a written mandate, an architecture update, and a recorded approval).

**Reading order for an epic:** the one `docs/plan/epics/E<nn>-*.md` spec you're executing → the `01-architecture.md` sections it cites → existing code in `crates/`. Ignore files bannered `SUPERSEDED`.

## Build & verify

```sh
export PATH="$HOME/.cargo/bin:$PATH"    # shell state doesn't persist between Bash calls
cargo xtask fixtures                     # one-time fixture fetch
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
cargo deny check                         # license gate (GPL never in-process)
```

Exit bar for ANY commit: all five green. Never commit broken. Record spec deviations in `docs/plan/epics/<EPIC>-deviations.md`.

## Guardrails (owner intent)

- Color/raw editing is the product; complete raw parameter surface on raw files; same UI minus raw stages otherwise.
- AI runs offline only. AI Looks proposals must be **editable recipes** (grading wheels/curves/HSL deltas), never baked filters; shuffle is seeded-deterministic.
- `cargo run -p lightbox-shell -- --smoke 60` exits after ~1 s **by design** (self-test, not a crash).
- Report honestly: failed gates are failures; skipped steps get named.
