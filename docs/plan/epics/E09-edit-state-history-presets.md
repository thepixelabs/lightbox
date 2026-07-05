# E09 — Edit state, history, presets, XMP (v2.0)

_Implementation spec, re-baselined for the v2.0 editing-first mandate. Author: staff engineer (epic planner). Inputs: `docs/plan/00-mandate.md` (**v2.1**), `docs/plan/01-architecture.md` (**v2.0**, decision-complete — §3.1.1, §3.2, §2.4, §10 govern this epic), `docs/plan/epics/E01-deviations.md` + the built code in `crates/` (spec'd against reality, see §2), research reports 02 (develop: history/snapshots/presets/sync), 05 (raw pipeline: process versions), 06 (XMP/export interop), 10 (landscape)._

| | |
|---|---|
| **Epic** | E09 `edit-state-history-presets` |
| **Milestone** | M1 (develop skeleton — this epic underwrites "edits auto-persist and survive `kill -9` and app restart") |
| **Effort** | M, ~4–6 pw (task breakdown sums to 28 dev-days ≈ 5.6 pw; spill points named in §5) |
| **Depends on** | **E01 only** (workspace, edit-store crate + migration framework + single writer + kill-9 harness, command bus, id newtypes, `ContentHash`). Soft seam to E06 for one task (T22 auto-write job class — graceful no-op fallback in tests). **No build dependency on E02/E04/E05** — per architecture §10 note, E09 builds and property-tests against the frozen §3.2 recipe schema before the engine or the working-set loader exist. |
| **Blocks** | E10 (consumes `Recipe`/`ParamDelta`), E08 (history panel, snapshots, preset browser, divergence badge, gesture wiring), E05 (recipe → graph params; `EditWorkingChanged` re-render trigger), E15 (reads recipes; export metadata packets), E12/E14 (extend mask/retouch reference lists + `SnapshotMaterializer`), E04 (open-time restore seam), E16 (`from_lr_crs` + fidelity-report machinery) |
| **License gate** | The ISO 16684 XMP Toolkit per-dependency **CTO sign-off is already GRANTED** (round-4 confirmation review, 2026-07-04, recorded in `02-approval.md`; architecture §12). Remaining work is plumbing only: cargo-deny entry + SBOM surface-2 registration (T13). |

---

## 0. Re-baseline: what changed from the v1.x spec (and why)

This document **replaces** the v1.x E09 spec entirely. The recipe/XMP *mechanics* and the §3.2 schema are unchanged; the epic's **role** changed. Deltas, each traced to the v2.0 architecture:

1. **The edit store is THE primary data model** (§3.1.1). v1.x framed edit state as one subsystem of a catalog-centric product; v2.0 makes `lightbox-catalog` an *internal edit store + cache index* and E09 is the epic that gives it its reason to exist. The load-bearing product promise this epic owns: **open a file → edit → close everything → reopen the same file (from anywhere, any path) → the recipe is back, with zero user action.** Identity is `ContentHash`; *path is a hint, not identity*.
2. **Auto-persist lifecycle** (§3.1.1) is spec'd explicitly (§4.2 below): every committed gesture is one WAL transaction; there is no Save, no "write changes?" prompt, and closing/replacing the working set can never lose a committed edit.
3. **XMP sidecar export is opt-in** (§3.1.1): the edit store is the single writer of record; sidecars are a projection, written only on explicit command or an opt-in (default **off**) auto-write preference.
4. **LR develop-settings import narrows** (§10 E16, Risk 9 v2.0 note): E09 still builds the `crs:` **read mechanism** (`from_lr_crs` + fidelity report — it is also what LR *preset* import consumes, and the same converter table backs our `crs:` compatibility *emit*), but the **product interop flow** — honoring an existing LR sidecar when a file is opened, legacy-PV coverage, user-facing fidelity surfacing — is **E16 (M4)**. E09's corpus obligation shrinks to modern-PV LR Classic sidecars/presets.
5. **The library-metadata XMP projection is cut with the DAM.** No `xmp:Rating`/`xmp:Label`/`dc:subject`/`lr:hierarchicalSubject`/IPTC mapping layer, no keyword semantics (v1.x T20): ratings/labels/keywords are keep-dormant tables (§3.1.1). Foreign metadata fields — including `dc:subject` et al. — **round-trip verbatim via `xmp_passthrough`**, exactly as §3.1.1 prescribes ("`dc:subject` still round-trips as passthrough via E09, not via these tables").
6. **No fs-watcher.** E07 (which owned it) is retired. Sidecar divergence is computed **at open time, at write time, and on explicit refresh** — never from a background watcher (§3.5 `sync` below). The `divergent` column becomes a computed status, not stored state.
7. **No virtual-copy API.** VC creation was E07 UI. The schema tolerates VCs (`image.is_virtual`); E09 ships no `clone_for_virtual_copy` and no command. Named versions are **snapshots** (a mandate Must).
8. **Spec'd against built code, not the v1.x sketches** (§2): `ProcessVersion` lives in `lightbox-types` (not redefined here); `Recipe` keeps the **E01-frozen field names `{ schema, pv }`** and `Recipe::identity()` (consumed today by `lightbox-render::RenderRequest`, `lightbox-core`, `lightbox-cli`); catalog timestamps are **TEXT RFC3339 fixed-width UTC** (E01 Phase-3 deviation), not unix-ms integers; core mutations extend the existing `#[non_exhaustive] Command` enum and `Queries` struct rather than inventing a parallel bus.

Everything else — CBOR recipe, param-delta engine, persistent history with keyframes, snapshots, ISO XMP Toolkit behind our own mapping layer, file-based presets, batched sync — carries over from v1.x with the adjustments above.

---

## 1. Scope

E09 builds everything between "a slider moved" and "a recipe is durably persisted, historized, presetable, and projectable to/from XMP" — with **no pixels involved**.

In scope:

1. **The versioned edit recipe** (§3.2 of the architecture): the full typed `Recipe` for the *entire* §3.2 field set (basic panel through effects — defined completely now so E10/E11 never bump the serialization schema for a field the architecture already enumerates), CBOR authoritative serialization into `edit_recipe.doc`, schema versioning + forward-compat (unknown-field preservation), canonical hashing.
2. **The param-delta engine**: typed param ids, deltas, diff/apply/extract by param group — the one mechanism that (research 02) buys presets, copy/paste, sync, history, and batch application "nearly for free".
3. **Edit persistence, the primary data model** (§3.1.1): `edit_recipe`/`edit_index`/`history_step`/`snapshot`/`xmp_sync` migration; the single-write-path transaction (doc + index + history in one txn); gesture-coalesced commits; the **content-hash-keyed restore chain** (file → `ContentHash` → `asset` → default `image` → `edit_recipe`) and the **auto-persist-on-open/edit lifecycle** (§4.2) — including the reopen-restores-recipe guarantee with the original moved or renamed.
4. **Persistent history + snapshots**: Lightroom-style step log (restore-to-step, truncate-on-edit-after-step-back, clear, undo/redo sugar), named snapshots (self-contained materialized projections, §3.1 ownership rule).
5. **Session + command surface**: `EditStore`/`EditSession` in `lightbox-edit`; an `EditHub` session registry in `lightbox-core` with a synchronous in-memory working-recipe path for the render seam; `Command::Edit(…)` variants and additive `Queries` methods on the existing seam-1 surfaces; headless-testable via `lightbox-cli`.
6. **XMP substrate** (`lightbox-meta`): the ISO 16684 XMP Toolkit (BSD-3; sign-off granted) behind Lightbox's own `XmpDoc` wrapper — packet parse/serialize, typed property access, atomic sidecar-only file I/O, read-only embedded-XMP extraction, untrusted-input hardening, edit-store↔sidecar divergence computation.
7. **The `crs:`/`lb:` mapping layer** (the part we own regardless of substrate, §1.6): documented field mapping, `Recipe::to_xmp()` (lb: full fidelity + crs: compatibility emit + foreign-field passthrough), `Recipe::read_xmp()` read-back, and the **`from_lr_crs()` mechanism** with a per-field fidelity report (modern PVs; product wiring is E16).
8. **Presets**: preset model + on-disk `.xmp` store, partial presets (group checklist), groups, create/apply/rename/delete/export, **import of Lightroom `.xmp` presets**, pure hover-preview function.
9. **Settings transfer**: copy/paste buffer with subset selection, batched sync-to-many (the mandate's "batch develop-settings application" + "apply a look across the set"), "Previous" — engine only; UI is E08/E10.

### 1.1 Explicit non-goals

- **No rendering, no pixels.** E09 never evaluates a recipe. `RenderNode::invalidates(&ParamDelta)` is E05's contract; we guarantee only that `ParamDelta` is a stable, inspectable type and that `Recipe::canonical_hash`/per-param hashing is deterministic (E05 cache keys, E03 cache keys).
- **No mask/retouch content.** `Recipe.masks`/`Recipe.retouch` are **ordered id lists only** (§3.1 ownership rule). The `mask`/`mask_component`/`retouch_op` tables and mask XMP serialization are E12/E14. E09 ships list plumbing, the `SnapshotMaterializer` seam (T10), and reserved `edit_index` columns — nothing more.
- **No `crs:` mask import.** `crs:MaskGroupBasedCorrections`/`crs:RetouchAreas` are preserved verbatim in `xmp_passthrough` and reported as skipped. E16 owns any later best-effort translation.
- **No library-metadata mapping.** Ratings/flags/labels/keywords/IPTC are cut with the DAM (§0 item 5). No `library_map` module exists in v2.0. Foreign fields ride passthrough.
- **No sidecar-honoring open flow, no legacy-PV LR import.** E09 ships the `from_lr_crs` mechanism tested against a modern LR Classic corpus; "open a file with an LR sidecar and honor it approximately" is the **E16/M4** interop flow (§9 M4). Pre-2012 (PV≤2) develop params are **skipped-with-report** in E09 (OQ4).
- **No `.lrcat` anything.** Retired with the DAM (E16 v2.0 note).
- **No virtual-copy commands** (§0 item 7).
- **No fs-watcher / watched folders** (§0 item 6).
- **No UI.** History panel, snapshot list, preset browser + hover preview, copy/paste checklist, divergence badge rendering, the auto-write preference toggle — all E08 shell work against E09's commands/queries.
- **No EXIF/IPTC binary parsing.** The probe/info-panel EXIF path is E04/E08 (`kamadak-exif` already lives in `lightbox-decode`). E09 touches only the XMP subsystem of `lightbox-meta`.
- **No embedded-XMP writing — ever, in any format.** Mandate constraint 2 (originals never modified) forbids writing XMP *into* JPEG/TIFF/DNG/HEIC originals. Lightbox writes **sidecar `.xmp` files only** and *reads* embedded XMP from all formats. (Exports are new files — E15 may embed there, consuming `to_xmp`.) Interop cost accepted and stated — see Risks.
- **No process-version migration UI.** We store and guard `pv`; only PV1 exists at M1. The migrate command rejects.
- **No ISO-adaptive/AI-adaptive presets, no preset amount slider** (Could/Won't tier; `lb_extra` leaves room).

### 1.2 The lifecycle picture (v2.0 — the one to keep in mind)

```
drop/open file (E04 loader)                                  slider drag (E08 shell)
   │ hash → asset row (find-or-create by ContentHash;             │ EditHub.begin/update (in-memory,
   │        path updated as a hint) → default image row           │ sync; Event::EditWorkingChanged)
   ▼                                                              ▼
EditStore::open_state(image)  ──────────────►  EditHub working Recipe ◄── render reads
   │  edit_recipe row?  yes → restored recipe                 │        working_recipe(image)
   │                    no  → neutral default (NO row write)  │        (E05 seam; never waits on DB)
   │                                                          │ commit_gesture (pointer-up)
   │                                                          ▼
   │                       ONE WAL txn: truncate steps > head_seq → append history_step
   │                         → upsert edit_recipe.doc/head_seq → rebuild edit_index row
   │                                     │ Event::EditCommitted{image, seq}
   ▼                                     ▼
divergence check (sidecar?)      queries: history, snapshots, is_edited badge (WAL readers)
   │                                     │
   └── badge (E08) ──── explicit ReadMetadata / WriteMetadata / opt-in auto-write (E06 Background)
                                         │
                                         ▼
                        Recipe::to_xmp() → XmpDoc (ISO toolkit) → atomic sidecar *.xmp
                        (opt-in projection; the edit store stays the single writer of record)
```

Failure modes owned here: torn edit txn (WAL invariant, kill-9 tested); working-set close/replace mid-gesture (auto-commit on close); sidecar write torn (atomic temp+fsync+rename); adversarial XMP input (fuzzed, capped); edit-store/sidecar divergence (computed, surfaced, never silently resolved); newer-schema doc read by an older build (read-only preservation, never rewrite-and-lose); moved/renamed originals (content-hash restore).

---

## 2. Crates / modules touched — against the real code

| Crate | Today (verified on `main`) | E09 work |
|---|---|---|
| **`lightbox-edit`** | 45-line stub: `Recipe { schema: u16, pv: ProcessVersion }` + `Recipe::identity(pv)`, `#[non_exhaustive]`, doc-marked "E09 replaces the body" | The epic's center of gravity: `params`, `recipe` (full §3.2 model — **keeping the frozen `schema`/`pv` field names and `identity()`**, which `lightbox-render::RenderRequest`, `lightbox-core::session` (via CLI) already consume), `history`, `store` (`EditStore`/`EditSession`), `snapshot`, `preset`, `transfer`, `xmp_map` |
| **`lightbox-meta`** | 8-line reserved stub ("owned by E07 and E09"; E07 retired → **E09 is now the sole M-phase owner**) | `xmp::doc` (XmpDoc over the ISO toolkit), `xmp::sidecar` (paths, atomic I/O, embedded read), `xmp::sync` (divergence). EXIF half stays empty (E04/E08 territory) |
| **`lightbox-catalog`** | Real: `Catalog` (WAL, `synchronous=NORMAL`, `foreign_keys=ON`), single-writer thread + `WriterHandle::with_txn` (one `BEGIN IMMEDIATE` txn per closure, panic-contained), forward-only embedded migrations (`MIGRATIONS` array + `docs/plan/migrations.md` registry + `cargo xtask lint-migrations`), DAOs on `CatalogTxn`, reader pool DTOs, RFC3339 fixed-width TEXT timestamps (`clock::now_rfc3339_utc`) | Migration `000N_edit_state.sql` (number reserved via registry PR at T5 start); write DAOs on `CatalogTxn` (`upsert_edit_recipe`, `append_history_step`, …); read surface on `ReaderHandle` (`edit_state_row`, `history_page`, `snapshots`, `edit_badges`, `image_for_content_hash`, `xmp_sync_row`). **All writes via the existing `WriterHandle`; no SQL crosses the crate boundary (E01 rule, kept)** |
| **`lightbox-core`** | Real: `#[non_exhaustive] Command` enum + dispatcher (`dispatch_loop`; trivial commands awaited in order on the blocking pool, long ones spawn as class-budgeted jobs), broadcast `Event`, `Queries` struct over `ReaderHandle`, clone-cheap `Session` with `previews()`/`engine()` accessors | `Command::Edit(EditCommand)` variant + dispatcher arm; `Event::{EditWorkingChanged, EditCommitted, XmpDivergenceChanged}`; additive `Queries` methods; **`Session::edits() -> Arc<EditHub>`** (mirrors the `previews()`/`engine()` accessor pattern); auto-commit of open gestures in `Session::close` (before the drain/backup, reusing the `InFlight` guard) |
| **`lightbox-cli`** | Real: hand-rolled subcommands `create/import/list/render/backup/check`, exit codes 0/1/2/3, uses `Recipe::identity(PV_M0)` for render | New subcommands: `edit`, `history`, `snapshot`, `preset`, `xmp` (§5 T11). Drives the §6 E2E. (E04 later renames `import`→`open`; E09 does not touch that seam) |
| **`lightbox-types`** | Real: `ImageId`/`AssetId`/`ContentHash` (+hex)/`ProcessVersion`/`PV_M0`, serde derives, "frozen surface — joint review" | **Additive only, flagged for the E01 joint-review rule:** `MaskId(pub i64)`, `RetouchOpId(pub i64)`, `SnapshotId(pub i64)`, `HistoryStepId(pub i64)` newtypes (shared vocabulary for E12/E14/E08) |
| **`lightbox-render` / `lightbox-shell`** | Consume `Recipe` (`RenderRequest.recipe`), `Recipe::identity` | **No E09 changes.** The `Recipe` body grows behind the frozen `{schema, pv}`/`identity()` surface; `#[non_exhaustive]` already prevents out-of-crate struct literals. A workspace compile is the proof (T2 AC) |

New third-party deps (all surface-1 allowlist-clean; the C++ toolkit registers on surface 2): `xmp_toolkit` (Adobe-published Rust bindings, MIT/Apache-2.0, vendoring the ISO 16684 XMP Toolkit C++, BSD-3, static), `ciborium` (MIT/Apache, CBOR), `uuid` (MIT/Apache, preset ids), `directories` (MIT/Apache, preset dir). Dev-only: `proptest` (already in workspace), `cargo-fuzz` target (workspace-excluded, per the `lightbox-decode/fuzz` precedent). **No `notify`/watcher dep** (§0 item 6; OQ5).

---

## 3. Interface definitions

Contracts, not implementations. Types named here are the seam artifacts other epics import.

### 3.1 Param vocabulary and deltas (`lightbox_edit::params`)

```rust
/// Stable wire ids. NEVER renumber; append only. Serialized as u16 in CBOR and history rows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
#[non_exhaustive]
#[repr(u16)]
pub enum ParamId {
    BaseProfile = 1,
    WhiteBalance = 10,
    Exposure = 20, Contrast = 21, Highlights = 22, Shadows = 23, Whites = 24, Blacks = 25,
    ToneCurve = 40,                       // composite + r/g/b, edited as one unit
    Hsl = 50, ColorGrade = 51, BwMix = 52, Vibrance = 53, Saturation = 54, Treatment = 55,
    Clarity = 60, Texture = 61, Dehaze = 62,
    Sharpen = 70, NoiseReduction = 71,
    LensProfile = 80, ChromaticAberration = 81, Defringe = 82, VignetteCorr = 83,
    Crop = 90, Angle = 91, Flip = 92, Upright = 93, Transform = 94,
    PostCropVignette = 100, Grain = 101, CreativeLut = 102,
    // structure (list-valued; content owned by E12/E14 tables — §3.1 arch ownership rule)
    MaskList = 120, RetouchList = 121,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ParamValue {
    F32(f32), I32(i32), Bool(bool),
    Wb(WhiteBalance), Curve(ToneCurveSet), Hsl(HslTable), ColorGrade(ColorGrade),
    BwMix(BwMix), Sharpen(Sharpen), Nr(NoiseReduction), Profile(ProfileRef),
    Crop(Crop), Upright(Upright), Transform(Transform), Lut(Option<CreativeLut>),
    Vignette(PostCropVignette), Grain(Grain), Lens(Optics), Treatment(Treatment),
    MaskIds(Vec<MaskId>), RetouchIds(Vec<RetouchOpId>),
}

/// Group taxonomy for preset checklists & copy/paste subsets (research 02: partial presets).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ParamGroup {
    BaseProfile, WhiteBalance, Tone, Curve, ColorMixer, ColorGrading, BwMix,
    Presence, Detail, Optics, Geometry, Effects, Masks, Retouch,
}
pub fn group_of(id: ParamId) -> ParamGroup;   // exhaustive match, no wildcard arm

#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParamSubset { pub groups: BTreeSet<ParamGroup> }

/// The universal currency: an ordered map of changed params → new values.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParamDelta(pub BTreeMap<ParamId, ParamValue>);

impl ParamDelta {
    pub fn merge(&mut self, later: ParamDelta);            // later wins per key
    pub fn restrict(&self, subset: &ParamSubset) -> ParamDelta;
    pub fn is_empty(&self) -> bool;
}
```

Laws (property-tested, T4): `a.apply(&b.diff(&a))` ⇒ `a == b`; `apply` idempotent per delta; `diff(a, a).is_empty()`; `restrict ⊆ subset`.

### 3.2 Recipe (`lightbox_edit::recipe`) — the §3.2 schema, typed, behind the E01-frozen surface

```rust
use lightbox_types::{ProcessVersion, MaskId, RetouchOpId};   // NOT redefined here (built reality)

pub const RECIPE_SCHEMA: u16 = 1;    // serialization schema; migrates independently of pv

#[non_exhaustive]                    // already so in the E01 stub — kept
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Recipe {
    // ---- E01-frozen fields: names and types unchanged (lightbox-render/cli compile untouched)
    pub schema: u16,
    pub pv: ProcessVersion,          // immutable per image (§4.5); asserted == image.process_version
    // ---- E09 body (architecture §3.2)
    pub base_profile: ProfileRef,
    pub global: GlobalStages,
    pub geometry: Geometry,
    pub masks: Vec<MaskId>,          // ordered refs ONLY (content: E12 tables)
    pub retouch: Vec<RetouchOpId>,   // ordered refs ONLY (content: E12 tables)
    pub lb_extra: BTreeMap<String, CborValue>,     // lb:-namespace extension bag
    pub xmp_passthrough: XmpPassthrough,           // foreign fields, preserved verbatim
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, CborValue>,      // forward-compat: keys a newer build wrote
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ProfileRef {
    pub kind: ProfileKind,           // Matrix (license-clean default, §1.7) | Dcp
    pub id: String,                  // "matrix-base" | curated/user profile id
    pub look_ref: Option<String>,    // Lightbox-authored default look family (§1.7 tier 2)
    pub look_amount: f32,            // 0.0..=2.0, default 1.0
}

/// Full §3.2 global set, typed NOW so E10/E11 add no schema fields. Defaults are neutral.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct GlobalStages {
    pub white_balance: WhiteBalance,     // AsShot | Auto | Preset(WbPreset) | Custom { temp_k, tint }
    pub exposure: f32,                   // stops, -5.0..=5.0
    pub contrast: f32, pub highlights: f32, pub shadows: f32,   // -100..=100
    pub whites: f32, pub blacks: f32,                           // -100..=100
    pub tone_curve: ToneCurveSet,        // rgb + r/g/b point curves, [0,1], ≤64 pts, monotone-x
    pub hsl: HslTable,                   // [HslBand; 8]: hue/sat/lum, -100..=100
    pub color_grade: ColorGrade,         // shadow/mid/high/global wheels + blend + balance
    pub treatment: Treatment,            // Color | BlackAndWhite
    pub bw: BwMix,                       // [f32; 8]
    pub vibrance: f32, pub saturation: f32,
    pub presence: Presence,              // clarity, texture, dehaze
    pub detail: Detail,                  // Sharpen{amount,radius,detail,masking}, Nr{luma,luma_detail,chroma,chroma_detail}
    pub optics: Optics,                  // lens_profile: Option<LensCorrection>, ca: bool, defringe, vignette_corr
    pub effects: Effects,                // postcrop_vignette, grain, creative_lut: Option<CreativeLut>
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Geometry { pub crop: Crop, pub angle: f32, pub flip: Flip,
                      pub upright: Upright, pub transform: Transform }

impl Recipe {
    /// E01-frozen constructor, kept: the neutral recipe (renders the source unmodified).
    /// Non-raw sources get the same neutral recipe; raw-only stages are neutral no-ops the
    /// §2.4 surface hides — SourceKind gates CONTROLS (E08), never the schema.
    pub fn identity(pv: ProcessVersion) -> Recipe;
    /// Neutral recipe seeded from a probe (as-shot WB, matrix base profile, full crop).
    pub fn default_for(pv: ProcessVersion, probe: &AssetProbe) -> Recipe;

    pub fn apply(&mut self, delta: &ParamDelta) -> Result<Applied, RecipeError>;  // validates + clamps
    pub fn diff(&self, from: &Recipe) -> ParamDelta;
    pub fn extract(&self, subset: &ParamSubset) -> ParamDelta;
    pub fn get(&self, id: ParamId) -> ParamValue;
    pub fn is_neutral(&self) -> bool;                    // drives edit_index.is_edited

    // serialization (T3)
    pub fn to_cbor(&self) -> Vec<u8>;
    pub fn from_cbor(bytes: &[u8]) -> Result<RecipeRead, RecipeError>;
    /// Deterministic encoding for hashing (struct-order fields, sorted maps): feeds
    /// xmp_sync stamps and is available to E03/E05 as a cache-key component.
    pub fn canonical_hash(&self) -> [u8; 16];            // xxh3-128 (twox-hash, E01 precedent)
}

/// Forward-compat read: a doc written by a newer build is preserved, never rewritten.
pub enum RecipeRead {
    Ok(Recipe),
    /// schema > RECIPE_SCHEMA: locked read-only; shell shows "edited in a newer Lightbox".
    NewerSchema { raw: Vec<u8>, schema: u16 },
}
```

### 3.3 Edit store, session, history, snapshots (`lightbox_edit::{store, history, snapshot}`)

```rust
/// The primary data model's API (§3.1.1). Wraps the E01 catalog handles.
pub struct EditStore { /* Arc<Catalog>: reader pool + WriterHandle */ }
impl EditStore {
    pub fn new(cat: Arc<Catalog>) -> EditStore;

    /// The open-time restore (§4.2 lifecycle). READ-ONLY: an untouched file
    /// creates no row (see §4.2 decision D1). Returns the persisted recipe,
    /// or the neutral default when none exists.
    pub fn open_state(&self, image: ImageId) -> Result<EditState>;

    /// Read-only recipe fetch (WAL reader) — E15 export, tooling.
    pub fn recipe_of(&self, image: ImageId) -> Result<RecipeRead>;

    /// The content-hash seam E04's loader calls after hashing a dropped file:
    /// the default image of the asset with this hash, if the store knows it.
    pub fn image_for_content_hash(&self, hash: ContentHash) -> Result<Option<ImageId>>;
}

pub struct EditState { pub recipe: Recipe, pub head_seq: u64, pub persisted: bool,
                       pub updated_at: Option<String> /* RFC3339, catalog convention */ }

/// In-memory working state; one per open image, owned by the core's EditHub (§3.4).
/// The render path reads `working()` — never the DB — during drags.
pub struct EditSession { /* image, working, committed, gesture: Option<Gesture> */ }
impl EditSession {
    pub fn working(&self) -> &Recipe;
    pub fn begin_gesture(&mut self, label: StepLabel);           // one gesture = one history step
    pub fn update(&mut self, delta: ParamDelta);                 // in-memory only; coalesced
    /// Builds the commit payload; the EditHub runs it as ONE WAL txn (§4.1 invariants).
    pub fn take_commit(&mut self) -> Option<PendingCommit>;
    pub fn cancel_gesture(&mut self);
    pub fn apply_committed(&mut self, state: EditState);         // after step_to/restore/sync
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StepLabel {
    Param(ParamId),                        // "Exposure"
    Preset { name: String },
    Paste, Sync, Reset,
    CrsImport,                             // E16 wiring; label reserved now
    XmpRead,
    SnapshotRestore { name: String },
    HistoryRestore { seq: u64 },
}

pub struct HistoryStepMeta { pub id: HistoryStepId, pub seq: u64, pub label: StepLabel,
                             pub ts: String, pub is_head: bool }

pub mod history {
    /// Newest-first, metadata only (E08 panel).
    pub fn list(cat: &Catalog, image: ImageId) -> Result<Vec<HistoryStepMeta>>;
    /// State at a step: nearest keyframe (every 64 steps) + delta replay. <10 ms @ 1k steps.
    pub fn recipe_at(cat: &Catalog, image: ImageId, seq: u64) -> Result<Recipe>;
    pub fn clear(cat: &Catalog, image: ImageId) -> Result<()>;   // keeps current doc; drops steps
}

pub struct SnapshotMeta { pub id: SnapshotId, pub name: String, pub ts: String }
/// E12 seam: snapshot docs must be SELF-CONTAINED (§3.1 — a snapshot is an immutable copy,
/// not a live owner). M1 default materializer is identity (mask/retouch lists are empty);
/// E12 registers one that inlines mask/retouch content into the snapshot doc.
pub trait SnapshotMaterializer: Send + Sync {
    fn materialize(&self, r: &Recipe) -> Result<CborValue>;
    fn restore(&self, doc: &CborValue, image: ImageId) -> Result<Recipe>;
}
```

### 3.4 Core surface (`lightbox-core`) — additive on the built seam-1 types

```rust
// lightbox_core::command — ONE new variant on the existing #[non_exhaustive] enum:
pub enum Command {
    /* … existing E01 variants unchanged … */
    Edit(EditCommand),
}

#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum EditCommand {
    // durable edit mutations (each = one WAL txn via the dispatcher, ordered like SetRating)
    CommitGesture   { image: ImageId },                    // drains EditHub's pending gesture
    ApplyPreset     { images: Vec<ImageId>, preset: PresetId },
    PasteSettings   { images: Vec<ImageId> },
    SyncSettings    { source: ImageId, targets: Vec<ImageId>, subset: ParamSubset }, // job (batched)
    ApplyPrevious   { images: Vec<ImageId> },
    ResetEdits      { images: Vec<ImageId> },
    StepTo          { image: ImageId, seq: u64 },
    Undo            { image: ImageId },                    // sugar: StepTo(head_seq - 1)
    Redo            { image: ImageId },                    // sugar: StepTo(head_seq + 1)
    ClearHistory    { image: ImageId },
    CreateSnapshot  { image: ImageId, name: String },
    RestoreSnapshot { image: ImageId, snapshot: SnapshotId },
    DeleteSnapshot  { image: ImageId, snapshot: SnapshotId },
    RenameSnapshot  { image: ImageId, snapshot: SnapshotId, name: String },
    // XMP (opt-in projection; develop scope only in v2.0)
    ReadMetadata    { images: Vec<ImageId> },              // explicit: sidecar → recipe (a history step)
    WriteMetadata   { images: Vec<ImageId> },              // explicit: recipe → sidecar
    SetAutoWriteXmp { enabled: bool },                     // pref lives in app prefs (E08), not catalog
    RefreshXmpStatus{ images: Vec<ImageId> },              // recompute divergence (no watcher, §0.6)
    // presets
    CreatePreset    { from: ImageId, name: String, group: Option<String>, subset: ParamSubset },
    ImportPresetFiles { paths: Vec<PathBuf> },             // LR or Lightbox .xmp
    DeletePreset    { preset: PresetId },
    RenamePreset    { preset: PresetId, name: String },
    ExportPreset    { preset: PresetId, dest: PathBuf },
}

// lightbox_core::event — additive variants on the existing #[non_exhaustive] Event:
pub enum Event {
    /* … existing … */
    /// In-memory working recipe changed (gesture update). NOT durable. The render
    /// scheduler re-renders from EditHub::working_recipe on this signal.
    EditWorkingChanged { image: ImageId },
    /// A durable edit txn committed (gesture, preset, paste, step_to, restore, reset…).
    EditCommitted { image: ImageId, seq: u64, label: StepLabel },
    /// Divergence status changed for an asset's sidecar (open/write/refresh-time).
    XmpDivergenceChanged { asset: AssetId, status: DivergenceStatus },
}

// lightbox_core::session — accessor in the previews()/engine() pattern:
impl Session {
    /// The edit hub: session registry + the synchronous working-recipe path.
    pub fn edits(&self) -> Arc<EditHub>;
}

/// DECISION D2 (in-epic authority; §5.1-threading conformant): gesture *updates* are
/// direct synchronous in-memory calls (a 60 Hz drag must not round-trip the command
/// queue), while every DURABLE mutation goes through the command bus as one WAL txn on
/// the E01 writer (run on the blocking pool, like run_txn_command today). working_recipe
/// hands out an Arc snapshot (atomic swap) so the render scheduler never takes the hub lock.
pub struct EditHub { /* EditStore + Map<ImageId, EditSession> + Arc<Recipe> snapshots */ }
impl EditHub {
    pub fn open(&self, image: ImageId) -> Result<()>;             // idempotent; open_state + register
    pub fn close(&self, image: ImageId);                          // auto-commits in-flight gesture
    pub fn working_recipe(&self, image: ImageId) -> Option<Arc<Recipe>>;   // render seam (E05/E10)
    pub fn begin_gesture(&self, image: ImageId, label: StepLabel) -> Result<()>;
    pub fn update_gesture(&self, image: ImageId, delta: ParamDelta) -> Result<()>; // emits EditWorkingChanged
    pub fn cancel_gesture(&self, image: ImageId) -> Result<()>;
    // commit itself is Command::Edit(CommitGesture) → dispatcher → one WAL txn → EditCommitted
}

// lightbox_core::queries — additive methods on the existing Queries struct (reader DTOs
// come from lightbox-catalog; Recipe hydration via lightbox-edit; no SQL crosses up):
impl Queries {
    pub fn edit_state(&self, image: ImageId) -> Result<Option<EditStateRow>>;   // doc, head_seq, updated_at
    pub fn edit_history(&self, image: ImageId) -> Result<Vec<HistoryStepMeta>>;
    pub fn snapshots(&self, image: ImageId) -> Result<Vec<SnapshotMeta>>;
    pub fn edit_badges(&self, images: &[ImageId]) -> Result<Vec<EditBadge>>;    // filmstrip: is_edited…
    pub fn xmp_status(&self, assets: &[AssetId]) -> Result<Vec<(AssetId, DivergenceStatus)>>;
    pub fn presets(&self) -> Vec<PresetMeta>;                                   // from the PresetStore
    pub fn preset_preview_recipe(&self, image: ImageId, preset: PresetId) -> Result<Recipe>; // pure
}
```

### 3.5 XMP substrate (`lightbox_meta::xmp`)

```rust
/// Thin, swappable wrapper over the ISO 16684 XMP Toolkit (via the Adobe-published
/// `xmp_toolkit` Rust bindings). The §1.6 reversal fallback (own quick-xml RDF)
/// re-implements exactly this API — nothing above xmp::doc may import toolkit types
/// (enforced by a CI grep, T13 AC).
pub struct XmpDoc(/* xmp_toolkit::XmpMeta */);

pub mod ns {
    pub const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
    pub const LB:  &str = "http://lightbox.app/ns/1.0/";       // registered prefix "lb"
    pub const XMP: &str = "http://ns.adobe.com/xap/1.0/";
    pub const DC:  &str = "http://purl.org/dc/elements/1.1/";
    pub const LR:  &str = "http://ns.adobe.com/lightroom/1.0/";
}

impl XmpDoc {
    pub fn new() -> XmpDoc;
    pub fn parse(packet: &[u8], limits: ParseLimits) -> Result<XmpDoc, XmpError>; // caps, T16
    pub fn serialize(&self) -> Result<String, XmpError>;       // canonical RDF/XML packet
    pub fn get(&self, ns: &str, path: &str) -> Option<XmpValue>;
    pub fn set(&mut self, ns: &str, path: &str, v: XmpValue) -> Result<(), XmpError>;
    pub fn delete(&mut self, ns: &str, path: &str);
    pub fn get_array(&self, ns: &str, name: &str) -> Option<Vec<XmpValue>>;      // Seq/Bag/Alt
    pub fn set_array(&mut self, ns: &str, name: &str, kind: ArrayKind, items: &[XmpValue]) -> Result<(), XmpError>;
    // struct/array paths addressed LR-style, e.g. "crs:ToneCurvePV2012[3]"
}

pub mod sidecar {
    /// `IMG_1234.CR3` → `IMG_1234.xmp` (LR-compatible; multi-extension collision policy
    /// documented: two same-stem originals share a sidecar — LR's own limitation, surfaced).
    pub fn sidecar_path(original: &Path) -> PathBuf;
    pub fn read(path: &Path) -> Result<Option<XmpDoc>>;
    /// Atomic: temp file + fsync + rename. NEVER writes into `original` (mandate c.2 guard).
    pub fn write_atomic(path: &Path, doc: &XmpDoc) -> Result<SidecarStamp>;      // hash + mtime
    /// Read-only embedded packet extraction (JPEG/TIFF/DNG/HEIC) via XMPFiles handlers,
    /// with a bounded packet-scan fallback for unhandled containers.
    pub fn read_embedded(original: &Path) -> Result<Option<XmpDoc>>;
}

pub mod sync {
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum DivergenceStatus {
        NoSidecar,       // no .xmp on disk
        SidecarOnly,     // sidecar exists, we never wrote/read it (foreign — E16's M4 entry point)
        InSync,          // disk == our last write AND recipe unchanged since
        CatalogNewer,    // recipe changed since last write; disk unchanged
        SidecarNewer,    // disk changed since our last write/read; recipe unchanged
        Conflict,        // both changed
    }
    /// COMPUTED, never stored (no fs-watcher in v2.0): compares the on-disk sidecar hash
    /// and the recipe canonical_hash against the xmp_sync stamps. Called at open, after
    /// write/read, and on EditCommand::RefreshXmpStatus.
    pub fn status(cat: &Catalog, asset: AssetId, original: &Path) -> Result<DivergenceStatus>;
}
```

### 3.6 Mapping layer (`lightbox_edit::xmp_map`)

```rust
pub struct XmpWriteCtx<'a> { pub app_version: &'a str /* provenance: lb:CreatorTool */ }

impl Recipe {
    /// Projection (read-only materialization, §3.1): full recipe → lb: namespace
    /// (schema-versioned, CBOR-faithful), PLUS best-effort crs: compatibility fields for
    /// LR readers, PLUS xmp_passthrough foreign fields re-emitted verbatim.
    pub fn to_xmp(&self, ctx: &XmpWriteCtx) -> Result<XmpDoc, XmpMapError>;

    /// Read back our own sidecar: lb: primary; crs:-only docs fall through to from_lr_crs.
    pub fn read_xmp(doc: &XmpDoc, probe: &AssetProbe) -> Result<RecipeFromXmp, XmpMapError>;

    /// The crs: read MECHANISM (Risk 9): crs: → Recipe + per-field fidelity report.
    /// Total: never fails on unknown fields — they land in xmp_passthrough + report.skipped.
    /// E09 wires it into preset import + explicit ReadMetadata; the sidecar-honoring OPEN
    /// flow and legacy-PV coverage are E16 (M4).
    pub fn from_lr_crs(doc: &XmpDoc, probe: &AssetProbe) -> CrsImport;
}

pub struct CrsImport { pub recipe: Recipe, pub report: CrsImportReport }
#[derive(Clone, Debug, Serialize)]
pub struct CrsImportReport {
    pub mapped: Vec<FieldFidelity>,        // 1:1 numeric mappings
    pub approximate: Vec<FieldFidelity>,   // domain-translated (tone curve, split-toning→grade, …)
    pub skipped: Vec<String>,              // e.g. crs:MaskGroupBasedCorrections, PV≤2 tone params
    pub source_pv: Option<String>,         // crs:ProcessVersion as written by LR
}
```

The **documented mapping table** ships as `docs/interop/crs-mapping.md` (T18): one row per field — Lightbox param, `crs:` property, value domains, conversion, fidelity class (exact / approximate / skipped), notes. Normative: converter unit tests are generated from it, and it is the deliverable Risk 9's "documented mapping" language requires. E16 extends it, never forks it.

### 3.7 Presets (`lightbox_edit::preset`)

```rust
#[derive(Clone, Debug)]
pub struct DevelopPreset {
    pub id: PresetId,                    // uuid v4 (lb:PresetId), stable across renames
    pub name: String,
    pub group: Option<String>,           // folder-derived
    pub subset: ParamSubset,             // which groups the preset carries
    pub delta: ParamDelta,               // partial values (only carried params)
    pub min_pv: ProcessVersion,
    pub origin: PresetOrigin,            // Lightbox | LightroomImport { source_pv: Option<String> }
    pub path: PathBuf,
}

/// DECISION D3 (kept from v1.x; architecture silent): presets are app-level file assets,
/// not catalog rows. Canonical store = one .xmp per preset under the platform config dir
/// (`<config>/Lightbox/presets/<group>/<name>.xmp`), LR-compatible on disk so LR presets
/// import by file and users share presets as files; the file format IS our to_xmp mapping
/// (dogfood). v2.0 makes this MORE right, not less: presets must outlive any one working
/// set, and the edit store is plumbing (§3.1.1), not a home for user-visible assets.
/// No directory watcher (§0.6): the index refreshes on open, after own mutations, and on
/// an explicit refresh (OQ5). Reversal: a catalog-side index table over the same files.
pub struct PresetStore { /* root dir + in-memory index */ }
impl PresetStore {
    pub fn open(root: PathBuf) -> Result<PresetStore>;    // cold scan; malformed files quarantined
    pub fn refresh(&self) -> Result<()>;
    pub fn list(&self) -> Vec<PresetMeta>;                // grouped, sorted
    pub fn get(&self, id: PresetId) -> Option<Arc<DevelopPreset>>;
    pub fn create_from(&self, recipe: &Recipe, name: &str, group: Option<&str>,
                       subset: &ParamSubset) -> Result<DevelopPreset>;
    pub fn import_files(&self, paths: &[PathBuf]) -> Vec<PresetImportResult>;   // LR or Lightbox .xmp
    pub fn rename(&self, id: PresetId, name: &str) -> Result<()>;
    pub fn delete(&self, id: PresetId) -> Result<()>;     // to OS trash where available
    pub fn export(&self, id: PresetId, dest: &Path) -> Result<()>;
}

/// Pure — no session, no commit, no store mutation. E08 hover preview uses this;
/// it is also the AI-Looks seam (E-future): a proposed look IS a DevelopPreset-shaped delta.
pub fn preview_recipe(base: &Recipe, preset: &DevelopPreset) -> Recipe;
```

### 3.8 Settings transfer (`lightbox_edit::transfer`)

```rust
pub struct CopiedSettings { pub subset: ParamSubset, pub delta: ParamDelta,
                            pub source_pv: ProcessVersion }

/// Sync engine — the mandate's "apply a look across the set" / batch develop-settings
/// application, bounded to the session working set (tens–hundreds; §7 v2.0 note).
/// Batched ~64 images per WAL txn; one history_step per image; edit_index rebuilt per row;
/// per-image progress events; CancelToken honored between chunks (E06 job when spawned).
pub fn sync_to(store: &EditStore, delta: &ParamDelta, targets: &[ImageId],
               label: StepLabel, cancel: &CancelToken) -> Result<SyncReport>;
```

---

## 4. Data model & lifecycle

### 4.1 Migration `000N_edit_state.sql` (`lightbox-catalog`)

Number reserved in `docs/plan/migrations.md` via PR at T5 start (registry rule; `cargo xtask lint-migrations` gates it). Applied through E01's forward-only runner (one txn per migration; copy-on-write pre-upgrade backup for existing stores). Conventions follow the **built** schema: TEXT RFC3339 fixed-width UTC timestamps (`clock::now_rfc3339_utc`), `foreign_keys=ON` is already set at connection open, no BEGIN/COMMIT in the file.

Planner-level refinement of §3.1's `history_step (op, params)` → `(op, delta, inverse, keyframe_doc)`: buys O(distance) history stepping and O(1) undo; called out here explicitly (unchanged from v1.x).

```sql
-- E09: edit state (the primary data model, §3.1.1), history, snapshots, xmp sync

CREATE TABLE edit_recipe (
  image_id    INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
  pv          INTEGER NOT NULL,          -- ProcessVersion; must equal image.process_version (§4.5)
  schema      INTEGER NOT NULL,          -- RECIPE_SCHEMA at write time
  doc         BLOB    NOT NULL,          -- CBOR Recipe (authoritative, §3.2)
  head_seq    INTEGER NOT NULL DEFAULT 0,-- history position `doc` corresponds to
  updated_at  TEXT    NOT NULL           -- RFC3339 UTC, fixed 6-digit subseconds (E01 convention)
);

CREATE TABLE edit_index (                -- derived; rebuilt in the SAME txn as doc (§3.1)
  image_id    INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
  is_edited   INTEGER NOT NULL DEFAULT 0,   -- filmstrip badge (§3.1.1: "edit_index badges the filmstrip")
  has_masks   INTEGER NOT NULL DEFAULT 0,   -- reserved; E12 populates
  has_ai_mask INTEGER NOT NULL DEFAULT 0,   -- reserved; E14 populates
  crop_ratio  REAL,
  treatment   TEXT,                          -- 'color' | 'bw' (B&W badge)
  updated_at  TEXT    NOT NULL
);

CREATE TABLE history_step (
  id           INTEGER PRIMARY KEY,
  image_id     INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  seq          INTEGER NOT NULL,         -- 1..n, dense per image; state at seq 0 = neutral default
  op           BLOB    NOT NULL,         -- CBOR StepLabel
  delta        BLOB    NOT NULL,         -- CBOR ParamDelta (forward)
  inverse      BLOB    NOT NULL,         -- CBOR ParamDelta (backward)
  keyframe_doc BLOB,                     -- full CBOR Recipe every 64th step, else NULL
  ts           TEXT    NOT NULL,
  UNIQUE (image_id, seq)
);

CREATE TABLE snapshot (
  id         INTEGER PRIMARY KEY,
  image_id   INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
  name       TEXT    NOT NULL,
  recipe_doc BLOB    NOT NULL,           -- SELF-CONTAINED materialized projection (§3.1)
  ts         TEXT    NOT NULL,
  UNIQUE (image_id, name)
);

CREATE TABLE xmp_sync (                  -- per-asset sidecar bookkeeping (§3.1.1 "sidecar sync state")
  asset_id        INTEGER PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
  sidecar_hash    BLOB,                  -- xxh3-128 of sidecar bytes at our last write/read
  sidecar_mtime   TEXT,                  -- disk mtime at that moment (cheap pre-check)
  recipe_hash     BLOB,                  -- Recipe::canonical_hash at that moment
  last_written_at TEXT,                  -- by us (WriteMetadata / auto-write)
  last_read_at    TEXT                   -- into the edit store (ReadMetadata)
  -- NOTE: no `divergent` column — status is COMPUTED (no watcher to keep stored state fresh)
);
```

**Write-path invariants** (enforced in the DAO/`EditStore`, fault-injection tested):

1. `edit_recipe.doc`, `edit_index`, and `history_step` mutate **only together, in one WAL txn**, through the E01 single writer (`WriterHandle::with_txn`). One write path; no divergence.
2. Commit protocol: `DELETE FROM history_step WHERE image_id=? AND seq > head_seq` → `INSERT` step (`seq = head_seq+1`; `keyframe_doc` every 64th) → `UPSERT edit_recipe (doc, head_seq, schema, updated_at)` → rebuild `edit_index` row. LR truncation semantics fall out of `head_seq`.
3. `pv` never changes in an UPDATE, and is asserted equal to `image.process_version` on every write (DAO assertion; migrate is a v1.x door).
4. Nothing outside `lightbox-catalog` issues SQL (E01 rule, kept: no SQL crosses up).
5. `xmp_sync` stamps update in their own txn *after* a successful atomic sidecar write; a crash between the two leaves the recompute-from-disk status correct (§3.5 `sync::status` derives, never trusts staleness).
6. Auto-write-XMP preference and the preset store are **not** catalog data (app prefs / disk files).

Storage estimate: doc ≈ 1–3 KB; steps ≈ 100–400 B + 1 keyframe/64. A session-scale store (thousands of images, not 100k) makes growth a non-issue; `ClearHistory` ships anyway; optional cap pref is OQ2.

### 4.2 The auto-persist lifecycle (§3.1.1, made concrete)

**Open** (E04 loader → core): hash the file (`lightbox-decode::hash_file`, canonicalization already matched to `ContentHash` pins) → find-or-create `asset` by `content_hash` — **path/filename update as a hint on the existing row when the file moved** — → find-or-create the default `image` row → `EditHub::open(image)` → `EditStore::open_state` returns the persisted recipe or neutral default → divergence check (`sync::status`) → events. Row find-or-create is E04's loader (its walker already batch-inserts through `insert_assets`); `image_for_content_hash` + `open_state` + the divergence call are the E09 seam it consumes.

**DECISION D1 — persist-on-first-edit, not on mere open.** `open_state` is read-only: an untouched file gets **no** `edit_recipe` row (a 500-file folder drop performs **zero** edit-store writes). The neutral default is derivable, so "nothing to restore" *is* the restored state. The first committed gesture creates the row (upsert). Rejected alternative — eager row per opened file: writes proportional to working-set size for no information, and it would make `is_edited` (a real badge) indistinguishable from "merely opened". **Invariant the tests pin:** after any committed edit, a durable row exists and `kill -9` at any later instant preserves it (§4.1-1); an edit is never held only in memory past its gesture commit, and session close / working-set replacement auto-commits any in-flight gesture (`EditHub::close`, `Session::close` before the drain — the "unsaved-nothing guarantee" of §2.4).

**Edit**: gesture updates are in-memory (D2); pointer-up commits one WAL txn (protocol §4.1-2); `Event::EditCommitted` fans out (render re-key, E03 cache hooks, E08 badge refresh, auto-write debounce).

**Reopen** (any path, any session): same content-hash → same `asset` → same `image` → `open_state` returns the recipe. Moving/renaming the file changes nothing (hash keying); editing the file's *bytes* (e.g. re-exported JPEG) is a **new** asset by construction — correct, since the recipe belonged to the old pixels.

**Project** (opt-in only): explicit `WriteMetadata`, or auto-write pref (default **off**) → debounced ≥2 s/image Background job → `to_xmp` → atomic sidecar → stamp txn. The edit store remains the single writer of record; a divergent sidecar is **never** silently read or overwritten without an explicit command (badge → user decides). `ReadMetadata` applies a sidecar to the recipe **as a history step** (`StepLabel::XmpRead`) — undoable, like everything else.

---

## 5. Ordered task breakdown (each ≤ 1 day)

**First failing tests of the epic (staff-engineer bar, §12):**
(a) `recipe_default_roundtrip` — `Recipe::default_for(PV1, probe)` → `to_cbor` → `from_cbor` equals; `canonical_hash` byte-identical across the 3-OS matrix (red until T3).
(b) `reopen_restores_recipe` — headless: open fixture → commit an exposure gesture → close session → **move the file to a new directory** → reopen by path → recipe equal, `head_seq` intact (red until T12; the §3.1.1 promise as one test).

### Phase A — recipe model & serialization (`lightbox-edit`) — 4 days

| # | Task | Acceptance criteria |
|---|---|---|
| T1 | Crate scaffold + param vocabulary: `ParamId` (stable u16 wire ids), `ParamValue`, `ParamGroup`, `group_of`, `ParamSubset`, `ParamDelta` merge/restrict. Add `MaskId`/`RetouchOpId`/`SnapshotId`/`HistoryStepId` newtypes to `lightbox-types` (additive; joint-review note in the PR per the E01 frozen-surface rule). | Wire-id snapshot test fails on any renumbering; every `ParamId` maps to exactly one group (exhaustive match); types-crate additions compile the whole workspace with no other change. |
| T2 | Typed `Recipe`/`GlobalStages`/`Geometry`/`ProfileRef` + all §3.2 leaf types, neutral defaults, `default_for(pv, probe)`, validation/clamping (ranges per §3.2; tone-curve monotone-x, ≤64 pts). **Keep the E01-frozen `{schema, pv}` field names and `identity(pv)`** (= neutral). | `identity(PV_M0).is_neutral()`; `default_for` neutral; out-of-range `apply` clamps and reports; curve invariants property-tested; **`lightbox-render`/`lightbox-core`/`lightbox-cli` compile unmodified** (frozen-surface proof). |
| T3 | CBOR serialization (`ciborium`): struct-order deterministic encode, `unknown`-map preservation, `RECIPE_SCHEMA` gate → `RecipeRead::NewerSchema` (read-only, never rewritten). | First failing test (a) green; proptest encode/decode identity over arbitrary recipes; docs with injected unknown keys round-trip byte-preserving; a newer-schema doc read then re-persisted leaves original bytes untouched. |
| T4 | Delta engine: `apply`/`diff`/`extract`/`get`, `canonical_hash` (xxh3-128 via `twox-hash`, big-endian canonicalization per the E01 pin convention). | Proptest laws (§3.1); hash identical across 3-OS CI; `canonical_hash` documented as an E03/E05 cache-key component. |

### Phase B — edit store, history, hub (the §3.1.1 core) — 8 days

| # | Task | Acceptance criteria |
|---|---|---|
| T5 | Reserve migration number (registry PR) + `000N_edit_state.sql` + write-DAO skeleton on `CatalogTxn` (`upsert_edit_recipe`, `append_history_step`, `truncate_history_after`, `rebuild_edit_index`, snapshot + xmp_sync CRUD), TEXT-RFC3339 timestamps via `clock`. | Migration applies to a fresh store and to an E01-fixture store (copy-on-write pre-upgrade dir appears); `lint-migrations` green; FK cascades verified (delete image → edit rows gone); pv-immutability assertion fires on a crafted violation. |
| T6 | Read surface + `EditStore`: `ReaderHandle::{edit_state_row, history_page, snapshots, edit_badges, image_for_content_hash, xmp_sync_row}` DTOs; `EditStore::{open_state, recipe_of, image_for_content_hash}`; `edit_index` rebuild fn (derives `is_edited`/`crop_ratio`/`treatment` from the doc). | D1 pinned: `open_state` on an untouched image creates no row (row-count probe); index row provably consistent with doc in the same txn (test hook); `image_for_content_hash` resolves across a simulated file move (new path, same bytes). |
| T7 | `EditSession` gesture lifecycle + commit protocol (§4.1-2) as a pure `PendingCommit` builder + the DAO txn that consumes it. | Slider-drag simulation (500 `update`s, 1 commit) writes exactly 1 step; commit p95 < 5 ms with 1k existing steps (criterion); working-recipe reads never touch the DB (poisoned-connection test double). |
| T8 | `EditHub` in `lightbox-core` (D2): registry, `Arc<Recipe>` working snapshots, `Session::edits()`, `Event::{EditWorkingChanged, EditCommitted}`, auto-commit on `EditHub::close` and in `Session::close` (before drain/backup). | Update → `EditWorkingChanged` within the same call stack (no bus round-trip); commit → `EditCommitted{seq}`; closing a session with an open gesture persists it (kill-after-close test); `working_recipe` is lock-free for readers (loom-style or contention smoke). |
| T9 | History reconstruction & navigation: keyframe-every-64, `recipe_at` (nearest anchor + replay), `StepTo` (persists doc+head_seq; later steps kept until next commit truncates), `Undo`/`Redo` sugar, `ClearHistory`. | `recipe_at` over a 1,000-step fixture < 10 ms; property test: `recipe_at(k)` ≡ brute-force replay for random gesture sequences; step-back-then-edit drops later steps exactly (LR semantics); undo/redo round-trips head_seq. |
| T10 | Snapshots: CRUD + `RestoreSnapshot` (itself a history step) + promote-history-step-to-snapshot + `SnapshotMaterializer` registry (M1 default = identity; E12 seam). | Create→mutate→restore yields struct-equal recipe; snapshot doc unchanged by later edits; per-image name uniqueness surfaced as a typed error. |
| T11 | Core + CLI wiring: `Command::Edit(EditCommand)` dispatcher arm (durable ops = one txn each, ordered like `SetRating`; `SyncSettings` spawns as a job), additive `Queries` methods, `lightbox-cli edit set/history/step-to/undo/snapshot …` subcommands (hand-rolled parser, exit-code conventions kept). | Headless CLI script: import fixture → `edit set exposure=+1.0` → `history` → `step-to` → `snapshot create/restore` all green; a malformed `edit set` exits 2; second `CommitGesture` with no open gesture is a typed no-op. |
| T12 | Auto-persist proof: extend the E01 kill-9 fault-injection harness with an edit-commit loop (journal protocol per the Phase-3 deviation notes); the `reopen_restores_recipe` E2E incl. file-move and file-rename legs; working-set-replacement auto-commit leg. | First failing test (b) green; 50-kill PR subset + 1000-kill nightly stay `integrity_check`-clean with zero lost *committed* gestures (journal-verified); at most the one uncommitted gesture is lost (§3.1.1 bound). |

### Phase C — XMP substrate (`lightbox-meta`) — 5 days

| # | Task | Acceptance criteria |
|---|---|---|
| T13 | Integrate `xmp_toolkit` (vendored ISO toolkit build) on the 3-OS matrix; license plumbing: cargo-deny entry, SBOM surface-2/native-inventory entry (BSD-3 static + MIT/Apache wrapper), link the **already-granted** CTO sign-off record (`02-approval.md`) in `docs/plan/licensing.md`'s follow-up commit; `XmpDoc::{new,parse,serialize}`; CI grep proving no toolkit type escapes `xmp::doc`. | 3-OS CI builds; parse→serialize of an LR-Classic sidecar fixture preserves all foreign properties (semantic diff empty); license surfaces green; encapsulation grep green. **Reversal trigger armed:** if any platform's build proves unmaintainable or T13 slips > 3 days, swap to own quick-xml RDF behind the same `XmpDoc` API (§1.6 fallback). |
| T14 | Typed property access: namespaces (`crs`/`lb`/`xmp`/`dc`/`lr`), scalars, Seq/Bag/Alt arrays, struct/array paths (`crs:ToneCurvePV2012[3]`), `lb:` prefix registration. | Read `crs:ToneCurvePV2012` + `lr:hierarchicalSubject` from fixtures; write/read-back every `XmpValue` kind. |
| T15 | Sidecar + embedded I/O: `sidecar_path` (collision policy documented), `write_atomic` (temp+fsync+rename), `read`, `read_embedded` (read-only; JPEG/TIFF/DNG/HEIC + bounded packet-scan fallback). | Fixtures per container; **byte+mtime-untouched assertion on the original for every write path (mandate c.2 guard — permanent test)**; torn-write simulation leaves old or new sidecar, never partial. |
| T16 | Untrusted-input hardening: `ParseLimits` (packet ≤ 16 MB, depth/property caps, UTF-8 validation), `cargo-fuzz` target (workspace-excluded, `lightbox-decode/fuzz` precedent) over `parse` + packet-scan, adversarial corpus, 5-min PR smoke + ≥1 h nightly. | Zero crash/hang/OOM; over-limit inputs → typed errors; triage doc committed (feeds the §12 security review of XMP parsing). |
| T17 | Divergence: `xmp_sync` stamps, `sync::status` (computed; §3.5 state machine incl. `SidecarOnly`), wiring at open / after write / after read / `RefreshXmpStatus`, `Event::XmpDivergenceChanged`. | External sidecar rewrite → `SidecarNewer` on next open/refresh; own `write_atomic` → `InSync`; edit-after-write → `CatalogNewer`; both → `Conflict`; batch status for a 500-file working set < 10 ms (stat+hash only when mtime moved). |

### Phase D — `crs:`/`lb:` mapping layer — 5 days

| # | Task | Acceptance criteria |
|---|---|---|
| T18 | `docs/interop/crs-mapping.md` (normative table) + table-driven converters (exposure stops, ±100 sliders, tone-curve 0–255→[0,1], WB modes, split-toning→color-grade, PV-string classification). | Doc covers every §3.2 global/geometry field with a fidelity class; converter unit tests generated from the table; masks/retouch + PV≤2 tone params listed as skipped-by-design with rationale. |
| T19 | `Recipe::to_xmp`: `lb:` full-fidelity emit (schema-versioned, CBOR-faithful), `crs:` compatibility emit, `xmp_passthrough` re-emission verbatim (incl. `dc:subject`/`lr:` foreign fields — §3.1.1 passthrough rule), provenance (`lb:Schema`, `lb:ProcessVersion`, app version). | Emitted packet parses clean in exiftool (dev-only subprocess oracle); contains mapped `crs:` fields per T18; injected foreign fixture fields present verbatim; full-packet snapshot golden. |
| T20 | `Recipe::read_xmp` (lb: primary, crs: fallback) + round-trip identity. | Proptest: arbitrary recipe → `to_xmp` → `read_xmp` → struct-equal (lb: path); crs:-only docs route to `from_lr_crs`; passthrough captures everything not ours. |
| T21 | `from_lr_crs` mechanism + `CrsImportReport`: modern-PV corpus (LR Classic PV5/6-era sidecars, freely licensed), legacy PV≤2 develop params → skipped-with-report (OQ4; E16 revisits). | Per-fixture expected mapped/approximate/skipped sets; `crs:MaskGroupBasedCorrections` skipped + preserved; total over fuzzed crs: property soup (never panics/errors); report serializes (E16/E08 seam). |
| T22 | `ReadMetadata`/`WriteMetadata` commands + opt-in auto-write: read = explicit, applies as a `StepLabel::XmpRead` history step, never auto; write = explicit or pref-gated debounced ≥2 s/image `Class::Background` job (E06; graceful synchronous fallback when jobs absent in tests); default **off** (§3.1.1). VC-bearing assets: only the non-virtual default image projects (documented). | CLI: edit → `xmp write` → wipe store → open → `xmp read` → recipe struct-equal + one XmpRead step; divergent sidecar never silently read (badge until explicit command); 50-commit burst coalesces to ≤ 2 writes. |

### Phase E — presets & settings transfer — 6 days

| # | Task | Acceptance criteria |
|---|---|---|
| T23 | `PresetStore`: config-dir layout (`directories`), group-as-folder, `lb:PresetId` identity, cold scan + `refresh`, malformed-file quarantine. | Cold scan of 500 files < 100 ms; quarantined file → typed error entry, store stays up; `refresh` picks up an externally dropped valid `.xmp`. |
| T24 | Create/rename/delete/export: `create_from(recipe, subset)` writes a partial `.xmp` (only checked groups' fields), rename/delete (OS trash where available), export-to-path. | Preset applied to neutral changes exactly the checked groups (property test over random subsets); export→import yields an equal preset. |
| T25 | Apply paths as history steps: `ApplyPreset` (one step, `StepLabel::Preset`), `PasteSettings`, `ResetEdits`, `ApplyPrevious`; pure `preview_recipe` (no side effects — store instrumentation check). | Apply → single labelled step; `Undo` restores exactly; multi-target apply = one step per image; `preview_recipe` leaves store/hub untouched. |
| T26 | LR `.xmp` preset import via `from_lr_crs`: present-key subset detection, PV normalization, per-file `PresetImportResult`; representative freely-licensed LR preset corpus. | Each corpus preset imports with the expected group subset; unmappable fields → report + dropped (presets carry no passthrough); a Lightbox-exported preset re-imports unchanged through the same path. |
| T27 | Copy/sync engine: core-held `CopiedSettings` buffer, `SyncSettings` batched (~64/txn), one step per target, progress events, `CancelToken` between chunks, `Previous` semantics (last-committed source). | Sync to 500 targets < 1 s warm (criterion) with 500 individual history steps; cancel mid-way leaves completed chunks committed, rest untouched, store consistent. |
| T28 | E2E + gates + polish: full `lightbox-cli` scenario (import fixtures → gestures → history walk → snapshot → preset create/apply → LR preset import → `xmp write` → fresh open → `xmp read` → equality + divergence states → kill-9 leg); criterion benches into the nightly perf harness; rustdoc pass; crs-mapping doc review closed. | Scenario green on 3-OS CI (fast subset PR-blocking per §8); perf ACs in the nightly dashboard; `cargo doc` clean; DoD checklist (§9) walked. |

**Total: 28 tasks ≈ 28 dev-days ≈ 5.6 pw** — inside the M (~4–6 pw) budget at the top of the band. Named spill points, each droppable to the epic tail without blocking dependents: T16 fuzz depth (keep the smoke, defer corpus growth), T21 corpus breadth (mechanism + 2 fixtures suffice for E16 to start), T26 (LR preset import is an M2 exit item, not M1).

Ordering constraints: A strictly before B and D; C parallel to B (different crates, second engineer if available); T18–T21 need T13–T15; E needs T4 + T19–T21; T22 needs T17; T11 can land after T8 with stubs for later commands.

---

## 6. Test plan

Per architecture §8; E09 rows PR-blocking unless noted.

**Unit (PR-blocking).** Param vocabulary invariants (T1); recipe defaults/validation/clamping (T2); every crs: converter against the mapping table (T18); sidecar path/collision policy (T15); divergence state machine incl. `SidecarOnly` (T17); preset partial semantics (T24); D1 no-row-on-open (T6).

**Property tests — `proptest` (PR-blocking; §8 names these explicitly).**
- `recipe → to_cbor → from_cbor ≡ recipe` (arbitrary recipes incl. extremes).
- Unknown-field preservation: CBOR docs with injected unknown keys and XMP packets with injected foreign properties survive read→write intact (the §3.2 forward-compat contract).
- Delta laws: `apply∘diff` identity; `extract ⊆ subset`; merge last-wins.
- `recipe → to_xmp → read_xmp ≡ recipe` (lb: path).
- `from_lr_crs` total over fuzzed crs: property soup; report partitions exhaustive (mapped ∪ approximate ∪ skipped covers every crs: key seen).
- History: `recipe_at(k)` ≡ brute-force replay; truncation preserves delta/inverse chain integrity.

**The §3.1.1 lifecycle suite (PR-blocking — the v2.0 headline).** `reopen_restores_recipe` incl. file-move + rename legs; working-set-replacement auto-commit; kill-9 fault injection over the edit-commit loop (50-kill PR subset, 1000-kill nightly, journal-verified zero-committed-loss / ≤-one-uncommitted-gesture bound); D1 zero-writes on a 500-file synthetic open.

**Fixture corpora (committed under `tests/fixtures/`).** Modern LR Classic sidecars (PV5/6-era); freely-licensed LR develop presets; our own emitted sidecar goldens (snapshot-tested); adversarial XMP (fuzz seeds); embedded-XMP containers (JPEG/TIFF/DNG/HEIC). Provenance/licensing recorded in the fixture manifest (E01 pattern).

**Fuzzing (5-min PR smoke; ≥1 h nightly).** `XmpDoc::parse`, embedded packet scan, `Recipe::from_cbor`. Zero crash/hang/OOM gate; feeds the §12 security review.

**Integration / E2E (fast subset PR-blocking; full nightly).** T28 CLI scenario; divergence workflow (external mutation → status → explicit read); auto-write debounce under commit storms; mandate-c.2 byte-identity guard on originals (permanent).

**Performance (`criterion`, nightly; regressions filed non-blocking per §8 + the lbx-perf baseline convention).** Commit txn p95 < 5 ms @ 1k-step history; `recipe_at` < 10 ms @ 1k steps; CBOR encode/decode < 50 µs typical; 500-target sync < 1 s; preset cold scan (500) < 100 ms; batch divergence (500 assets) < 10 ms. Rationale: the <100 ms slider budget (§7) requires edit writes off the render path and tiny txns.

**Golden-image tests: none** (no pixels). E09's analogue is the sidecar/CBOR snapshot goldens; render goldens over recipes are E05/E10's gate consuming our fixtures.

**Cross-platform (PR-blocking).** Everything above on macOS/Windows/Linux; the C++ toolkit build (T13) and canonical-hash determinism (T4) are the platform-sensitive spots.

---

## 7. Risks & open questions

### Risks

| # | Risk | Mitigation / owner |
|---|---|---|
| R1 | **XMP Toolkit build/integration weight** (C++/CMake under cargo, 3 OSes) exceeds the "bounded FFI-integration cost" bet (§1.6/Risk 9). | Adobe-published `xmp_toolkit` bindings, not hand FFI (T13). Hard reversal named by the architecture and armed in T13: own quick-xml RDF **behind the same `XmpDoc` API**; encapsulation enforced by CI grep so the swap is one-crate. Trigger: T13 > 3 days or an unmaintainable platform build. |
| R2 | **crs: fidelity expectations** (Risk 9): users read "reads LR sidecars/presets" as "renders identically". | `CrsImportReport` is first-class; the mapping doc classifies every field; `lb:` provenance recorded; passthrough keeps originals. v2.0 additionally *narrows the promise*: the open-flow interop ships at M4 (E16) with expectation-setting UI — E09 never silently applies a foreign sidecar. |
| R3 | **Sidecar-only writing** (mandate c.2) breaks interop with tools that only read embedded XMP in JPEG/DNG. | Accepted, stated (user docs + mapping doc). Read side covers embedded fully; exports embed via E15 (new files). A guarded opt-in embedded write is a possible v1.x mandate-owner escalation — out of E09. |
| R4 | **Content-hash keying misses "same photo, new bytes"** (re-exported/retouched-elsewhere files silently lose their recipe association). | By design (§4.2: the recipe belonged to the old pixels) — but *stated*, and the old asset row + recipe remain restorable. If user reports show this bites, a filename+capture-time heuristic relink (the §6 relink machinery) is the named, additive fix — E16 territory, no schema change. |
| R5 | **Two-writer temptation**: E12's mask baking writing through recipe paths. | Ownership rule restated in code: `Recipe.masks` is ids-only; the DAO exposes no API to embed content; snapshot self-containment goes through `SnapshotMaterializer` (T10) so E12 extends without touching `edit_recipe.doc` semantics. |
| R6 | **Schema-freeze pressure from E10/E11/E12**: a field the architecture didn't enumerate arrives mid-M2. | `lb_extra` + `unknown` absorb additive fields without a schema bump; structural changes bump `RECIPE_SCHEMA` with a documented `from_cbor` n→n+1 upgrade; wire-id discipline (T1) makes additions cheap. |
| R7 | **Gesture-commit granularity** wrong (too many/few history steps) hurts UX and history size. | Gesture boundaries owned by the shell (pointer-down→up, field blur) against an explicit, testable `begin/update/commit` API; auto-commit-on-close guards dropped gestures; E08 integration week validates feel. |
| R8 | **EditHub lock contention** between 60 Hz gesture updates and render-scheduler reads at M1 integration. | D2's `Arc<Recipe>` snapshot swap makes reads lock-free by construction; T8 AC includes a contention smoke. If snapshot cloning of large recipes ever shows in profiles, per-param dirty tracking is a hub-internal change (no seam movement). |
| R9 | **History table growth** under heavy editing. | Session-scale store (not 100k-catalog scale, §7 v2.0); delta rows small, keyframes sparse; `ClearHistory` ships; cap pref reserved (OQ2). |

### Open questions (none block start; owners + due points named)

1. **OQ1 — `edit_badges` delivery to the filmstrip**: separate batch query (spec'd) vs. joining `edit_index` into `ImageSummary` (a frozen-DTO change). Decide with E08 owner at M1 integration week; both ride the same `edit_index` row.
2. **OQ2 — History cap preference** (default unlimited, LR-matching): ship in E08's prefs panel or defer entirely? Decide with E08 by M1 feature-freeze; schema needs nothing either way.
3. **OQ3 — `xmp_passthrough` storage form**: full original packet bytes (zstd, chosen default) vs. pruned foreign-only subtree. T20 measures typical sizes; revisit only if p95 packet > 64 KB.
4. **OQ4 — Legacy pre-2012 (PV≤2) crs: tone params**: Adobe's 2010→2012 conversion is unpublished. E09 ships skipped-with-report; E16 decides best-effort numeric translation at M4 with corpus evidence. (Narrowed from v1.x, per the re-spec mandate.)
5. **OQ5 — Preset-directory watching**: v2.0 drops the watcher dep; `refresh` on browser-open + after own mutations covers v1. If E08 wants live external-drop pickup, `notify` (license-verify) is an additive E08-time decision behind `PresetStore::refresh`.
6. **OQ6 — Sidecar policy for virtual copies** if/when VCs return: current rule (only the default image projects) is documented in T22; revisit with whichever epic revives VC creation.
7. **OQ7 — `SidecarOnly` UX at M1–M3**: badge-only (spec'd) until E16 ships the honor-on-open flow. Confirm with E08 that the badge copy sets the "not applied yet" expectation.

---

## 8. Seams to neighboring epics (named, not designed)

| Epic | Seam artifact E09 provides | What the neighbor owns |
|---|---|---|
| **E01** | consumes: migration framework + registry, `WriterHandle::with_txn`, reader pool, command bus + events, kill-9 harness, `ContentHash`/id newtypes, RFC3339 clock | foundation semantics (unchanged) |
| **E04** (working-set loader) | `image_for_content_hash`, `EditStore::open_state`, `EditHub::open/close`, open-time `sync::status` call | hashing/probing dropped files, find-or-create asset/image rows, path-hint update on move, working-set ordering (§2.4) |
| **E05/E10** | `Recipe` (frozen `{schema, pv}` surface), `ParamDelta`, `canonical_hash`, `EditHub::working_recipe`, `Event::{EditWorkingChanged, EditCommitted}` | `RenderNode::invalidates(&ParamDelta)`, recipe→DAG planning (replaces the M0 `RenderPlanner` scaffolding), render scheduling; E10 panels emit gestures |
| **E03** | `EditCommitted` + `canonical_hash` as cache-key inputs for rendered preview tiers | preview pyramid, raw cache, invalidation policy |
| **E08** (editor shell) | `Queries::{edit_history, snapshots, edit_badges, xmp_status, presets, preset_preview_recipe}`, `EditCommand` vocabulary, typed errors, gesture API (D2) | history panel, snapshot UI, preset browser + hover preview, copy/paste checklist, divergence badge, prefs (auto-write toggle, OQ2), gesture boundaries, keymap |
| **E12/E14** | `Recipe.masks`/`retouch` id lists, `MaskList`/`RetouchList` param ids, `SnapshotMaterializer` registry, reserved `edit_index` columns | mask/retouch tables + content, baked-raster policy (§4.5), mask XMP serialization |
| **E15** | `recipe_of`, `Recipe::to_xmp` for exported-file metadata packets (new files — embedding allowed there) | encoders, metadata-filtering tiers, batch export |
| **E16** | `from_lr_crs` + `CrsImportReport` + mapping doc + `SidecarOnly` status + `StepLabel::CrsImport` | the M4 honor-LR-sidecar-on-open flow, legacy-PV coverage (OQ4), interop hardening, user-facing fidelity surfacing |
| **E06** | auto-write + sync spawned as `Class::Background`/`Foreground` jobs with `CancelToken` | scheduler, activity surfacing |

---

## 9. Definition of done

E09 is done when **all** of the following hold:

1. All 28 tasks' acceptance criteria pass on the 3-OS CI matrix; PR-blocking suites (unit, property, lifecycle, fixtures, fast E2E, fuzz smoke, kill-9 fault injection) green.
2. **The §3.1.1 promise works headlessly**: via `lightbox-cli` alone — edit an image's params with history/snapshots persisted crash-safely; close; **move the file**; reopen and get the identical recipe back with no user action; `kill -9` at any point loses at most one uncommitted gesture and never corrupts the store (`integrity_check` clean).
3. **The M1 exit behaviors this epic underwrites hold**: basic-panel edits auto-persist and survive `kill -9` + app restart (M1 exit line, verbatim); presets create/apply incl. an imported LR preset; sync applies a look across a working set; sidecars write on explicit command and read back struct-equal into a fresh store with correct divergence states.
4. **XMP is provably opt-in and subordinate**: with the pref off, zero sidecar writes occur under any edit workload (asserted); a divergent sidecar is never auto-read or overwritten; the original-file byte-identity guard (mandate c.2) is a permanent PR-blocking test.
5. The §8 test-strategy rows owned by E09 (recipe/XMP property tests, unknown-field preservation, crs: import fidelity) exist in CI and are PR-blocking.
6. **License gates**: cargo-deny green with the new deps; the XMP Toolkit registered in the surface-2 SBOM/native-inventory with the recorded CTO sign-off linked; no GPL anywhere in the E09 dependency set (exiftool = dev-time oracle subprocess only, never shipped).
7. `docs/interop/crs-mapping.md` published and reviewed (E16 owner + one pixel-stream engineer), every §3.2 field classified; the E09→E16 hand-off note states what interop is and isn't shipped at M1.
8. Perf numbers recorded in the nightly harness with the T7/T9/T27 targets met on the reference machine; no unresolved perf regressions filed against E09.
9. Public APIs rustdoc'd; ownership rules (single write path, ids-only mask refs, projection-not-owner XMP, D1/D2/D3 decisions) restated in crate-level docs; OQ1–OQ7 each resolved or explicitly re-owned by a named epic.
10. Zero UI work shipped (boundary held); E04/E05/E08/E10/E15/E16 planners have confirmed the §8 seam artifacts are sufficient to start; the `lightbox-render`/`lightbox-shell` crates compiled throughout without E09 modifications (frozen-surface proof).
