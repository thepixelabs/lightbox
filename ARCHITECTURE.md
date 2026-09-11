# Architecture

This is a map of the workspace and the handful of decisions that shape everything built on top of them. It records the load-bearing decisions a new contributor needs before touching any of it: what is where, and why it is that way rather than the obvious alternative.

## Crates and tools

| Crate | What it owns |
|---|---|
| `lightbox-types` | Shared id and error newtypes with no behaviour, breaking the dependency cycle between the catalog and the core so leaf crates don't need to depend on the core to share a type. |
| `lightbox-catalog` | The crash-proof SQLite edit store: one writer thread, one WAL transaction per mutation, a pool of read-only WAL-snapshot connections for everything else. |
| `lightbox-core` | The headless command/query/event façade. No UI type is allowed to cross down into it, no SQL type is allowed to cross up out of it. |
| `lightbox-decode` | File probing, content hashing, and permissive in-crate container parsing (TIFF-IFD, ISO-BMFF) for raw and rendered files. Raw mosaic decode itself lives elsewhere. |
| `lightbox-rawproxy` | A standalone binary: the out-of-process, feature-gated LibRaw decode sandbox. The only place LGPL C++ code runs, and it never links into the app. |
| `lightbox-color` | Camera-matrix colour science, DCP evaluation, white balance, and ICC colour management. Pure math plus an LCMS2 FFI binding. |
| `lightbox-render` | The GPU compute node-graph render engine (wgpu/WGSL), with a CPU fallback path for every node. |
| `lightbox-render-testkit` | The shared golden-image and performance-harness infrastructure every pixel-producing crate tests against. Dev/test only; never shipped. |
| `lightbox-edit` | The versioned edit recipe, the parameter-delta engine, history, snapshots, presets, and the XMP field-mapping layer. Holds recipes, never pixels. |
| `lightbox-meta` | Reading and writing XMP sidecars behind an `XmpDoc` API. The architecture names the Adobe ISO 16684 XMP Toolkit as the intended substrate; what ships today is our own RDF/XML implementation over `quick-xml` behind that same API, which is why the API exists. Knows nothing about what a recipe is. |
| `lightbox-ingest` | Discovery, probing, hashing, and batched catalog inserts for opening files in place. No managed import, no copy/rename. |
| `lightbox-preview` | Demand-driven preview provision behind one trait, with the tiered on-disk preview pyramid and raw decode cache behind it. |
| `lightbox-jobs` | A cancellable job system over a tokio runtime, with three priority classes so interactive work never queues behind background work. |
| `lightbox-export` | The core export pipeline: open, edit, render, encode to JPEG/TIFF/PNG, single image or a small batch. A deliberate slice of a larger planned export system. |
| `lightbox-mask` | Reserved, empty. Owned by the not-yet-built masking, local-adjustment, and retouch epic. |
| `lightbox-ml` | Reserved, empty. Owned by the not-yet-built local ONNX inference client. |
| `lightbox-shell` | The egui/eframe editor shell: canvas, filmstrip, develop panels, preferences. The only crate permitted to depend on egui, eframe, or winit. |
| `lightbox-cli` | A headless driver over `lightbox-core`. CI asserts its dependency tree carries no UI crate at all. |

| Tool | What it does |
|---|---|
| `tools/xtask` | The workspace task runner: fixture fetch, the migration and native-dependency lints, macOS app bundling, and the kill-9 exit drill. |
| `tools/lbx-perf` | The performance-scenario harness: import, page-query, and navigation benchmarks compared against committed baselines. |
| `tools/lbx-image-compare` | The golden-image comparator (CIEDE2000 + PSNR) and its bless workflow, used by every crate with pixel output. |
| `tools/lightbox-profgen` | An internal, unshipped tool that drives `dcamprof` (GPL-3) as a subprocess to generate curated camera colour profiles from target shots. Never links into the product. |

## Four decisions everything else rests on

### One shared wgpu device between the engine and the UI

The render engine and the egui shell hold the same `Arc<wgpu::Device>`. The rendered output is composited into the UI as just another render pass in the same frame, with no texture copy between them.

The alternative, a separate GPU context for the toolkit and one for the engine, is what most cross-platform imaging tools do, and it costs a per-frame texture handoff plus a cross-API synchronisation surface that becomes a standing source of platform-specific bugs. Sharing one device removes that cost entirely, at the price of coupling the shell to whatever GPU abstraction the engine uses. That coupling is deliberate: `lightbox-core` stays headless specifically so the shell can be swapped later (for a different toolkit, if egui's ceiling is ever hit) without touching the engine, which keeps the expensive, hard-to-reverse part of this bet (the shared device) narrowly scoped to one crate boundary.

### Content-hash identity, not file paths

Every asset in the edit store is keyed by the content hash of its bytes; the file path is stored as a hint for the UI, never as identity. A file can be renamed, moved to a different folder, or copied, and Lightbox still recognises it and reattaches its edit recipe on next open.

This matters because Lightbox does not manage where your files live: there's no import step that copies files into a library folder, so the files you open stay wherever your OS and your workflow put them, and they move around outside Lightbox's knowledge all the time. Identity by path would mean a renamed file loses its edit history the moment Lightbox can't find the old path; identity by content hash means it doesn't, at the cost of a hash computation on open and of two identical files (a real duplicate, or a copy) sharing one edit history unless you explicitly diverge them.

### Recipes, not pixels, versioned by process version

An edit is never a pixel buffer. It's an ordered, serializable set of parameters (a `Recipe`), rendered against the untouched original by a node graph tagged with the `process_version` that was current when the image was first opened. The `NodeRegistry` keeps every process version's node implementations registered forever; a later improvement to, say, highlight recovery ships as a new process version rather than changing the old one's behaviour underfoot.

The reason is the same one that motivates Adobe's own process-version mechanism: without it, an algorithm improvement silently changes the render of every photo ever edited, and a photographer who dialled in a look years ago finds it looks different today for no action they took. Freezing old versions in place means storage cost (every process version's code stays compiled in) in exchange for a guarantee that's otherwise very expensive to retrofit: an edit made today renders the same way years from now. Golden-image tests run per process version specifically to catch any accidental drift in an old version's output before it ships.

### Decode runs out of process

Raw mosaic decoding through LibRaw happens in a separate binary (`lightbox-rawproxy`), spawned as a subprocess and talking to the rest of the app over a small message protocol, dynamically linked to LibRaw only when the `libraw` feature is explicitly enabled.

Two independent reasons landed on the same answer here. First, licensing: LibRaw is LGPL, and dynamic-linking it from an isolated process, rather than statically linking it into the app or depending on it as a Rust crate, is what keeps the default build free of copyleft code without asking users to accept a licence they didn't choose. Second, and just as important on its own: LibRaw is memory-unsafe C++ parsing raw files, which are untrusted binary input from the moment a user drags one in from an unknown camera or a file someone else sent them. A crash or a memory-safety bug in that decoder, in a sandboxed subprocess with its own resource limits, fails one file and gets reported; the same bug linked into the main process takes the whole editing session down, mid-edit, with whatever wasn't yet committed to the edit store at risk. Isolating decode costs an IPC round trip per decode and a second binary to build and ship; both are cheap next to either failure mode it avoids.

## Where a new thing goes

- **A new develop node** (a new pixel operation in the render graph) goes in `crates/lightbox-render/src/nodes/`, as its own module alongside `display_transform.rs`, following that node's pattern: a WGSL kernel, a CPU fallback sharing the same algorithm spec, and an entry in the `NodeRegistry`. Register the module in `crates/lightbox-render/src/nodes/mod.rs`.
- **A new develop panel** (a UI control surface in the shell) goes in `crates/lightbox-shell/src/panels/`, alongside `basic.rs`, `curve.rs`, `hsl.rs`, and the rest, and gets registered with the `PanelDef` host in `panels/host.rs`. Panels declare a `min_source_kind` so raw-only controls hide themselves on non-raw sources automatically; read how `min_source_kind` is used by the existing panels before adding a raw-only control.
- **A new CLI subcommand** goes in `crates/lightbox-cli/src/`, either as a new top-level match arm in `main.rs` for a new noun, or as a new arm inside an existing module (`edit.rs`, `export.rs`, `open.rs`, `preview.rs`) for a new verb on an existing one. It should call into `lightbox-core`'s existing command/query surface rather than reimplement anything; if the command you need doesn't exist on that surface yet, that's a `lightbox-core` change, not a CLI one.
- **A new migration** goes in `crates/lightbox-catalog/migrations/`, as the next sequential `NNNN_<name>.sql` file, with its number reserved first in `docs/reference/migrations.md` (the source of truth `cargo xtask lint-migrations` checks against). Migrations are forward-only and run inside one transaction each; there are no down-migrations, so a mistake is fixed by a following migration, not by editing history.
