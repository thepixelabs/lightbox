# Contributing to Lightbox

Thanks for wanting to work on this. It's a Rust workspace with a broad license-hygiene discipline layered on top of the usual build/test/lint cycle, because the project ships under a dual-licensing model (see [Licensing](README.md#licensing)); most of the process below exists to keep that model honest rather than to slow you down for its own sake.

## The exit bar

Every commit on `main` passes all five of these. Run them in this order; each one is cheap to fix early and expensive to discover late.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check
```

`cargo xtask fixtures` needs to have been run at least once first (it fetches the hash-pinned CC0 raw corpus the test suite reads); see the README's build section for the toolchain pin and the rest of the setup.

Two more gates run in CI alongside those five and are worth running locally before you push, because they fail fast and fail clearly:

```sh
cargo xtask lint-migrations     # checks docs/plan/migrations.md against crates/lightbox-catalog/migrations/
cargo xtask lint-native-deps    # checks Cargo.lock's *-sys crates against native-inventory.toml
```

`lint-migrations` exists because migration numbers are reserved by PR before the SQL is written, so two people working in parallel never collide on the same number; the lint just confirms the registry and the migrations directory agree. `lint-native-deps` exists because `cargo deny` only sees the Rust crate graph, and a native `-sys` crate can bundle or bind to C/C++ code that never shows up there; every `-sys` crate has to be classified in `native-inventory.toml` before it can land, which is the point at which its licence actually gets looked at.

Never commit with a red gate. If a gate is failing for a reason unrelated to your change, say so in the PR rather than working around it.

## Commit messages

There's no conventional-commits scheme here (`feat:`, `fix:`, etc.). The convention in the history is a plain, descriptive sentence in imperative or present tense, usually naming the user-visible symptom before the fix, for example:

```
Resolve the embedded ICC profile so P3/AdobeRGB files stop rendering flat
Fix straighten and crop failing to render: the app's registry lacked the E11 geometry nodes
Stop 25 presets blowing out the image: fix the Highlights/Whites signs
```

A doc-only change occasionally gets a `docs:` prefix, but that's a loose habit, not an enforced format. What matters is that someone reading `git log --oneline` a year from now can tell what changed and why without opening the diff.

## The deviations log

Every epic spec lives at `docs/plan/epics/E<nn>-*.md`. When an implementation departs from that spec, whether by cutting scope, choosing an interface the spec left open, or discovering the spec was wrong, the departure gets a dated entry in `docs/plan/epics/<EPIC>-deviations.md`, appended, never rewritten. This exists because the alternative is silent drift: a spec that nobody updates and an implementation that nobody explains, until the two disagree and no one remembers why. The deviations log is what lets a later reader (or reviewer) trust that "spec says X" and "code does Y" are either the same thing, or a documented, reasoned departure. If you implement part of an epic, add to that file. If you're fixing a bug that reveals the spec and the code already disagreed, record it there too, rather than quietly matching whichever one you found.

## Dependency licensing policy

This is enforced by `cargo deny check` (surface 1: the Rust crate graph) and `cargo xtask lint-native-deps` (surface 2: native `-sys` crates), and it has two parts:

- **Permissive only in the Rust crate graph.** `deny.toml` carries an exhaustive allowlist (MIT, BSD-2/3-Clause, Apache-2.0, Zlib, Unicode-3.0, ISC, CC0-1.0, plus a short list of named per-crate exceptions each with a comment explaining why). Anything copyleft, including LGPL, fails this check by construction. Adding a dependency outside the allowlist means either finding a permissive alternative or arguing for a scoped exception in a licensing-review PR that updates `deny.toml` and `docs/plan/licensing.md` together.
- **Copyleft native libraries only out-of-process or dynamically linked.** LGPL code (LibRaw, lensfun, libheif/libde265) never becomes a Rust crate dependency; it's either dynamically linked from a build script under an opt-in feature (so the default build links nothing native), or, where the code is memory-unsafe C/C++ parsing untrusted input, isolated in its own subprocess. GPL and AGPL tools (dcamprof, ArgyllCMS) run only as subprocesses invoked at build- or content-generation time, never linked into anything that ships.

**The nuance worth understanding:** this policy is driven by the commercial licence, not by the AGPL. PixeLabs needs to be able to offer Lightbox under a separate commercial licence to companies that can't accept the AGPL's obligations, and it can only make that offer for code it can license on those terms. If GPL or LGPL code from a third party were linked into the product, PixeLabs would have no right to relicense that code commercially, no matter what licence the rest of Lightbox carries. So this constraint does not relax now that the project itself is AGPL rather than permissively licensed; it stays exactly as strict as it always was, because it was never about avoiding copyleft for its own sake, it was about preserving the ability to dual-license.

## Where specs live, and the order to read them in

- `docs/plan/00-mandate.md`: the fixed constraints, what Lightbox is, what it explicitly is not, and the non-negotiables (local-first, non-destructive, license hygiene).
- `docs/plan/01-architecture.md`: the approved system design, written ADR-style with alternatives and reversal triggers per decision.
- `docs/plan/epics/E<nn>-*.md`: the spec for the piece of work in front of you.
- `docs/plan/epics/<EPIC>-deviations.md`: what actually shipped for that epic, and where it departed from the spec above.
- `docs/plan/03-handoff.md`: the current state of the whole project, what's built, what's specced, and what's next. Start here if you're not sure which epic you're touching.

Then, and only then, the code in `crates/`. Ignore anything bannered `SUPERSEDED`.

## The CLA

Contributing requires signing the CLA at `CLA.md`. The reason is mechanical, not a matter of distrust: because PixeLabs offers Lightbox under two licences (the AGPL for everyone, and a separate commercial licence for companies that need one), it needs the right to distribute your contribution under both. A contribution submitted under the AGPL alone, with no further grant, could go into the AGPL codebase but not into the commercial one, since nobody but you would hold the right to relicense it. The CLA obtains that permission once, at contribution time, instead of PixeLabs needing to track you down for a fresh grant every time the commercial offering ships a release that includes your change.

## Good first contributions

A few areas are genuinely self-contained and don't require deep familiarity with the render engine or the edit-store internals:

- **Wire up a CLI subcommand that the core already supports.** `lightbox-cli edit` currently exposes only `set` and `get` (`crates/lightbox-cli/src/edit.rs`), but `Command::Edit` in `lightbox-core` already has variants for sync, paste-settings, previous, and reset that have no CLI surface yet. Adding one of these is a matter of following the existing `set`/`get` pattern in that file: parse the arguments, dispatch the existing command, print the result the way its siblings do.
- **Write a crate README.** Only `crates/lightbox-preview/README.md` exists today. A short README for another crate, in the same shape (what it owns, what it explicitly doesn't, where to start reading), is low-risk and genuinely useful to the next person who opens that crate cold.
- **Add a built-in preset.** Dial in a look in the running editor, then export it with `lightbox-cli preset export` to produce the `.xmp` sidecar, and add it under `assets/presets/<Group>/`. You'll also need a matching entry in `assets/presets/MANIFEST.toml` recording its provenance; `crates/lightbox-edit/tests/preset_library.rs` checks the two stay in sync, so a mismatch fails loudly and locally rather than in review.

For anything larger, especially anything touching the render engine, the edit-recipe schema, or an epic marked "spec ready" but not built in `docs/plan/03-handoff.md`, open an issue describing what you want to do before writing code. Several of those areas have architectural decisions still pending, and it's cheaper to find that out before the PR than after.
