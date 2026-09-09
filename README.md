# Lightbox

An open-source, local-first raw photo editor.

Lightbox is editing-first: colour correction, tone, curves, grading and geometry, applied non-destructively to raw and rendered images. Local adjustments, retouch and on-device AI are intended and are not built yet. There is no library and no catalogue of your photos. You drag a file, a folder, or a multi-file selection onto the window, or open one from the OS dialog or the command line, and you land straight in the editor with a filmstrip of whatever you opened. Close the app and that session is gone; reopen the same file later and your edit comes back, because it was never stored in a library, it was stored against the file's content hash. This is a deliberate cut, not a missing feature: earlier plans for Lightbox included a Lightroom-style catalogue, collections, keywords, and culling, and all of that was removed from scope in favour of depth in colour and editing.

## Status

Lightbox is pre-1.0 and under active development. The foundation (workspace, crash-safe edit store, render engine, editor shell, edit history and presets) is built and exercised end to end, including a kill-9 crash-safety drill. The global develop toolset is landing panel by panel. Masking, retouch, and local AI are not built. Treat everything below as a snapshot, not a promise; nothing here is aspirational, but a fast-moving project drifts, so if you rely on something specific, check the code.

## What works today

- Opening photos: drag-and-drop of a single file, a multi-file selection, a folder, or a folder tree (recursive), the OS open dialog, command-line arguments, and a live folder explorer for browsing into nested folders. Every path lands in one editor with a session filmstrip; none of them build or persist a library index.
- A GPU compute node-graph render engine (wgpu, written in WGSL) that shares one device with the UI for zero-copy display, with a CPU fallback path so editing keeps working, at preview resolution, if the GPU is unavailable or lost.
- A develop rail covering white balance, exposure and tone, presence (clarity, texture, dehaze), a parametric and point tone curve, an 8-band HSL colour mixer, three-way colour grading, and basic crop and straighten geometry. Panels dock, collapse, and rearrange, and remember how you left them.
- Non-destructive editing throughout: every change is a versioned, undoable recipe, not baked pixels, stored in a local SQLite edit store keyed by content hash rather than by file path. History, named snapshots, and a survives-`kill -9` guarantee are all real and tested against real crashes, not just claimed.
- A curated built-in preset library, plus saving, applying, importing, and exporting your own presets, and a picker for installing third-party creative LUT packs (`.cube`, HaldCLUT).
- Optional XMP sidecar read and write, mapped onto Adobe's `crs:` fields where one exists and a separate `lb:` namespace for what that schema doesn't cover. The edit store is always the source of truth; the sidecar is a projection you opt into.
- Cancellable background jobs, so importing, previewing, and rendering never block the UI thread.
- Batch export to JPEG, PNG, or TIFF, from the shell's export dialog or from a headless CLI (`lightbox-cli`) that also drives store creation, folder browsing, editing, XMP status, backup, and integrity checks with no dependency on the UI at all.

## What is not built yet

- Masking, local adjustments, and retouch (heal, clone, remove). `lightbox-mask` exists in the workspace only as an empty crate reserving the name.
- Any local AI: denoise, super-resolution, AI-assisted masking, and the out-of-process ONNX inference host. `lightbox-ml` is likewise an empty reserved crate.
- AI Looks, the image-adaptive cinematic grading engine described in the project's own planning documents as a core feature, is specced but its build status could not be confirmed from those documents at the time of writing. Do not assume it works.
- The clean-room demosaic algorithm and the rest of the "complete raw parameter surface" goal: highlight reconstruction from raw latitude, per-channel camera calibration, raw-domain denoise, and lens/geometry correction beyond crop and straighten.
- The rest of export: watermarking, an external-editor round trip, JPEG XL/AVIF/DNG output, filename templates, collision handling, and export presets.
- XMP/Lightroom interop hardening beyond the basic sidecar read/write above.
- Any library, catalogue, or index of your photos: no persistent database of images, no collections, no keywords, no ratings, no cross-shoot search. This is cut on purpose, not pending.

## Screenshots

![Lightbox editor with a raw photo open in the develop view](web/assets/img/app-editor.webp)

## Building it

Lightbox is a Rust workspace. The toolchain is pinned in `rust-toolchain.toml`; any `cargo`/`rustup` invocation in the repo picks up 1.96.1 with `rustfmt` and `clippy` automatically. If `cargo` isn't already on your `PATH`:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Fetch the pinned test-fixture corpus once (a small, hash-verified set of CC0 raw files used by the test suite):

```sh
cargo xtask fixtures
```

Then the five gates that every commit here has to pass, in the order CI runs them:

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check
```

Run the editor:

```sh
cargo run -p lightbox-shell
```

`cargo run -p lightbox-shell -- --smoke 60` is a self-test, not the app: it boots the shell headlessly and exits after about a second. That exit is by design, not a crash; it exists so CI can prove the shell still boots on three platforms without a human watching a window.

On macOS, package a real app bundle:

```sh
cargo xtask bundle-mac        # target/macos/Lightbox.app
cargo xtask bundle-mac --install   # and copy it into /Applications
```

### The LibRaw caveat

Raw mosaic decoding (turning sensor data into an image, as opposed to reading a file's embedded JPEG) goes through LibRaw, which is LGPL-licensed. To keep the default build free of copyleft code in-process, LibRaw is not linked into Lightbox itself: it runs in a separate sandboxed process (`lightbox-rawproxy`), dynamically linked, and only if you build with the feature on:

```sh
cargo build -p lightbox-rawproxy --features libraw
```

**Without that feature, raw files still open, but through their embedded preview JPEG rather than a full sensor-data render.** This matters: it means the default build cannot show you what your raw file actually looks like once you start pushing white balance, highlight recovery, or anything else that depends on the real mosaic data. A packaged, distributable build of Lightbox needs `lightbox-rawproxy` built with `--features libraw` and LibRaw present on the build machine; the plain workspace build is a development convenience, not the shipping configuration.

## Repository layout

| Path | Contents |
|---|---|
| `crates/` | The Rust workspace: engine, edit store, decode, colour, shell, CLI, and the reserved-but-empty masking/ML crates |
| `tools/` | `xtask` (the task runner used above) and the golden-image/perf/profile-generation tools that support it |
| `docs/plan/` | The mandate, the approved architecture, per-epic specs, and the deviations logs recording what shipped versus what was specced |
| `assets/` | Bundled, licence-tracked content: colour profiles/looks and the built-in preset library |
| `fixtures/` | The hash-pinned CC0 test corpus fetched by `cargo xtask fixtures` |
| `web/` | The project website |

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for how the pieces fit together and why, and [`CONTRIBUTING.md`](CONTRIBUTING.md) for how to get a change in.

## Licensing

Lightbox is free software, published under the GNU Affero General Public
License, version 3 or, at your option, any later version, with an attribution
requirement added under section 7(b) of that licence and set out in
COPYING.additional-terms.

If you are a photographer, that licence asks nothing of you. Use Lightbox for
personal work, for study, and for paid client work. Modify it if you like.
Running the software, modified or not, creates no obligation of any kind.

The licence matters if you intend to build Lightbox, or any part of it, into a
product you distribute under terms of your own, or to offer it to your users
as a hosted or networked service. In those cases the AGPL requires you to
release the source of the resulting work under the same terms. Many companies
are glad to do that. If yours is not, PixeLabs offers Lightbox under a separate
commercial licence that lifts the copyleft obligation, and which can also carry
written warranties, an indemnity and a support commitment.

PixeLabs holds the copyright in Lightbox and is therefore free to licence it on
other terms. Third-party components keep their own licences under either
arrangement. To discuss commercial terms, write to licensing@pixelabs.net.
