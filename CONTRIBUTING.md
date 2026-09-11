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
cargo xtask lint-migrations     # checks docs/reference/migrations.md against crates/lightbox-catalog/migrations/
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

When an implementation departs from what a module's doc comment or a test's rationale says, change both in the same commit. This matters more than it sounds: the alternative is silent drift, a comment nobody updates next to code nobody explains, until the two disagree and no one remembers which was right. If you are fixing a bug that turns out to be a case the comment already got wrong, say so there too, rather than quietly matching whichever one you found first.

## Dependency licensing policy

This is enforced by `cargo deny check` (surface 1: the Rust crate graph) and `cargo xtask lint-native-deps` (surface 2: native `-sys` crates), and it has two parts:

- **Permissive only in the Rust crate graph.** `deny.toml` carries an exhaustive allowlist (MIT, BSD-2/3-Clause, Apache-2.0, Zlib, Unicode-3.0, ISC, CC0-1.0, plus a short list of named per-crate exceptions each with a comment explaining why). Anything copyleft, including LGPL, fails this check by construction. Adding a dependency outside the allowlist means either finding a permissive alternative or arguing for a scoped exception in a licensing-review PR that updates `deny.toml` and `docs/reference/licensing.md` together.
- **Copyleft native libraries only out-of-process or dynamically linked.** LGPL code (LibRaw, lensfun, libheif/libde265) never becomes a Rust crate dependency; it's either dynamically linked from a build script under an opt-in feature (so the default build links nothing native), or, where the code is memory-unsafe C/C++ parsing untrusted input, isolated in its own subprocess. GPL and AGPL tools (dcamprof, ArgyllCMS) run only as subprocesses invoked at build- or content-generation time, never linked into anything that ships.

**The nuance worth understanding:** this policy is driven by the commercial licence, not by the AGPL. PixeLabs needs to be able to offer Lightbox under a separate commercial licence to companies that can't accept the AGPL's obligations, and it can only make that offer for code it can license on those terms. If GPL or LGPL code from a third party were linked into the product, PixeLabs would have no right to relicense that code commercially, no matter what licence the rest of Lightbox carries. So this constraint does not relax now that the project itself is AGPL rather than permissively licensed; it stays exactly as strict as it always was, because it was never about avoiding copyleft for its own sake, it was about preserving the ability to dual-license.

## A note on references to `docs/plan/`

Around 86 files carry comments citing paths like `docs/plan/epics/E02-deviations.md`.
Those documents are the project's internal planning and deviation log, and they are
not published. The citations are left in place deliberately rather than stripped,
because each one marks a decision that was argued somewhere and records that the
code in front of you is deliberate rather than accidental. If a comment cites one
and you need the reasoning behind it, open an issue and ask: the answer is usually
a paragraph, and it is more useful written into the comment than left in a document
nobody can read.

## Where to read, and the order to read it in

- [`ARCHITECTURE.md`](ARCHITECTURE.md): the map. What is in each crate, the seams between them, and the decisions everything else rests on.
- [`docs/engine-book/`](docs/engine-book/): the render engine in detail, including a walkthrough of adding a node.
- [`docs/interop/crs-mapping.md`](docs/interop/crs-mapping.md): how every recipe field maps onto Adobe's `crs:` XMP namespace, which is what makes sidecars readable elsewhere.
- [`docs/reference/migrations.md`](docs/reference/migrations.md): the migration registry `cargo xtask lint-migrations` checks against. Reserve your number here first.

Then the code. Crates carry module level doc comments that explain why a thing is shaped the way it is, not just what it does, so `cargo doc --open` is worth the minute it costs.

## The CLA

Contributing requires signing the CLA at `CLA.md`. The reason is mechanical, not a matter of distrust: because PixeLabs offers Lightbox under two licences (the AGPL for everyone, and a separate commercial licence for companies that need one), it needs the right to distribute your contribution under both. A contribution submitted under the AGPL alone, with no further grant, could go into the AGPL codebase but not into the commercial one, since nobody but you would hold the right to relicense it. The CLA obtains that permission once, at contribution time, instead of PixeLabs needing to track you down for a fresh grant every time the commercial offering ships a release that includes your change.

## Good first contributions

A few areas are genuinely self-contained and don't require deep familiarity with the render engine or the edit-store internals:

- **Wire up a CLI subcommand that the core already supports.** `lightbox-cli edit` currently exposes only `set` and `get` (`crates/lightbox-cli/src/edit.rs`), but `Command::Edit` in `lightbox-core` already has variants for sync, paste-settings, previous, and reset that have no CLI surface yet. Adding one of these is a matter of following the existing `set`/`get` pattern in that file: parse the arguments, dispatch the existing command, print the result the way its siblings do.
- **Write a crate README.** Only `crates/lightbox-preview/README.md` exists today. A short README for another crate, in the same shape (what it owns, what it explicitly doesn't, where to start reading), is low-risk and genuinely useful to the next person who opens that crate cold.
- **Add a built-in preset.** Dial in a look in the running editor, then export it with `lightbox-cli preset export` to produce the `.xmp` sidecar, and add it under `assets/presets/<Group>/`. You'll also need a matching entry in `assets/presets/MANIFEST.toml` recording its provenance; `crates/lightbox-edit/tests/preset_library.rs` checks the two stay in sync, so a mismatch fails loudly and locally rather than in review.

For anything larger, especially anything touching the render engine, the edit-recipe schema, or anything the README lists as not built yet, open an issue describing what you want to do before writing code. Several of those areas have architectural decisions still pending, and it's cheaper to find that out before the PR than after.
