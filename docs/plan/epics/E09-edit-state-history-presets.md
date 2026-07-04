# E09 — Edit state, history, presets, XMP

_Implementation spec. Author: staff engineer (epic planner). Inputs: `docs/plan/00-mandate.md` (v1.1), `docs/plan/01-architecture.md` (approved; decision-complete), `docs/research/00-feature-catalog.md`, research reports 01/02/05/06/10._

| | |
|---|---|
| **Epic** | E09 `edit-state-history-presets` |
| **Milestone** | M1 |
| **Effort** | M, ~4–6 pw (task breakdown below sums to ~27 dev-days ≈ 5.4 pw) |
| **Depends on** | E01 (workspace, catalog + migration framework, command bus, writer handle, kill-9 harness). Soft seam to E06 for one task (T21 auto-write job class). **No build dependency on E05/E02** — per §10 of the architecture, E09 builds against the frozen §3.2 recipe schema, not the render engine. |
| **Blocks** | E10 (consumes Recipe/ParamDelta), E15 (reads recipes, emits export metadata packets), E16 (`.lrcat` importer reuses `from_lr_crs`), E12/E14 (extend recipe's mask/retouch reference lists), E04 (apply-on-import preset, sidecar read on import), E07 (write-metadata command, divergence badge), E08 (history panel, preset browser UI) |
| **Gate** | The ISO 16684 XMP Toolkit dependency requires the per-dependency **CTO license sign-off** (mandate v1.1 / architecture §12). Routine, not a planning blocker; tracked as an acceptance item on T11. |

---

## 1. Scope

E09 builds the **edit/interop stream's foundation**: everything between "a slider moved" and "a recipe is durably persisted, historized, presetable, and projected to/from XMP" — with **no pixels involved**.

In scope:

1. **The versioned edit recipe** (§3.2): the full typed `Recipe` model for the *entire* §3.2 field set (basic panel through effects — defined completely now so E10/E11 never bump the serialization schema for a field the architecture already enumerates), CBOR authoritative serialization (`edit_recipe.doc`), schema versioning + forward-compat (unknown-field preservation), canonical hashing.
2. **The param-delta engine**: typed param ids, deltas, diff/apply/extract by param group — the one mechanism that (per research 02) buys presets, copy/paste, sync, history, and batch "nearly for free".
3. **Edit persistence + history**: `edit_recipe`/`edit_index`/`history_step`/`snapshot` tables (migration), the single-write-path transaction (doc + index + history in one txn), gesture-coalesced commits, persistent Lightroom-style history (restore-to-step, truncate-on-edit-from-earlier-step, clear, promote-to-snapshot), named snapshots.
4. **Session + command surface**: `EditSession`/`EditStore` in `lightbox-edit`, `EditCommand`/`EditQuery` on the `lightbox-core` command bus, headless-testable via `lightbox-cli`.
5. **XMP substrate** (`lightbox-meta`): the ISO 16684 XMP Toolkit (BSD-3, mandate v1.1) behind Lightbox's own `XmpDoc` wrapper — packet parse/serialize, typed property access, sidecar file I/O (atomic, sidecar-only writes), read-only embedded-XMP extraction, untrusted-input hardening, catalog↔sidecar divergence detection.
6. **The `crs:`/`lb:` mapping layer** (the part we own regardless of substrate): documented field mapping, `Recipe::to_xmp()` (lb: full fidelity + crs: compatibility emit + foreign-field passthrough), XMP→Recipe read-back, **one-way `from_lr_crs()` Lightroom import** with a per-field fidelity report.
7. **Library-metadata XMP projection**: rating/label/keywords (`xmp:Rating`, `xmp:Label`, `dc:subject`, `lr:hierarchicalSubject`), title/caption, IPTC Core — as pure mapping functions plus the `ReadMetadata`/`WriteMetadata` commands E07's UI writes through.
8. **Presets**: preset model + on-disk `.xmp` store, partial presets (group checklist), groups, create/apply/rename/export, **import of Lightroom `.xmp` presets**, hover-preview support (pure function, no commit).
9. **Settings transfer**: copy/paste buffer with subset selection, sync-to-selection (batched), auto-sync broadcast API, "Previous" — engine only; UI is E08/E10.

### 1.1 Explicit non-goals

- **No rendering, no pixels.** E09 never evaluates a recipe. E05/E10 consume `Recipe`/`ParamDelta`. The `RenderNode::invalidates(&ParamDelta)` contract is E05's; we only guarantee `ParamDelta` is a stable, inspectable type.
- **No mask/retouch content.** `Recipe.masks`/`Recipe.retouch` are **ordered id lists only** (§3.1 ownership rule). The `mask`/`mask_component`/`retouch_op` tables, their semantics, and mask XMP serialization are E12/E14. E09 ships the list plumbing, the snapshot self-containment hook (T9), and reserves the schema field — nothing more.
- **No `crs:` mask import.** `crs:MaskGroupBasedCorrections`/`crs:RetouchAreas` are *not* mapped in E09 (masks don't exist until M3). They are preserved verbatim in `xmp_passthrough` and reported as skipped. E16 owns any later best-effort translation.
- **No `.lrcat` catalog importer.** That is E16; it reuses `from_lr_crs` and the report machinery built here.
- **No UI.** History panel, preset browser + hover preview widget, copy/paste checklist dialog, before/after views, divergence badge rendering are E08/E10 shell work against E09's queries/commands.
- **No EXIF/IPTC binary parsing.** `kamadak-exif` integration and the `metadata_cache` population live with E04 (ingest probe) / E07 (viewer). E09 touches only the XMP subsystem of `lightbox-meta`.
- **No embedded-XMP writing — ever, in any format.** Mandate constraint 2 ("originals are never modified") forbids writing XMP *into* JPEG/TIFF/DNG originals. Lightbox writes **sidecar `.xmp` files only** (darktable's model), and *reads* embedded XMP from all formats. The interop cost (tools that only read embedded XMP won't see our edits on JPEG/DNG) is accepted and stated — see Risks.
- **No ISO-adaptive / AI-adaptive presets, no preset amount slider** (catalog: Could/Won't). Schema leaves room (`lb_extra`), no v1 engineering.
- **No process-version migration UI** (catalog: Could). We store and guard `pv`; only one PV exists at M1. The migrate command is stubbed to reject.
- **No soft-proofing, no Quick-Develop relative batch ops** (E10/E08 later; the delta engine supports relative ops but the command is not built here).

### 1.2 Data flow (the one picture to keep in mind)

```
slider drag (shell)                          LR sidecar / preset .xmp
   │ EditCommand::Gesture{delta}                       │
   ▼                                                   ▼
lightbox-core command bus ──► EditSession.working (in-memory Recipe)
   │                              │▲  render reads working recipe (E05 seam;
   │ commit_gesture()             ││   never waits on DB)
   ▼                              ▼│
one WAL txn: edit_recipe.doc ◄─ ParamDelta engine ◄─ from_lr_crs / preset apply
  + edit_index rebuild
  + history_step append (truncate > head_seq first)
   │
   ├─► queries: history list, snapshot list, is_edited badge (WAL readers)
   └─► WriteMetadata (explicit or opt-in auto-write, Background job)
            │
            ▼
     Recipe::to_xmp() + library-metadata mapping ──► XmpDoc (ISO toolkit)
            │                                            ▲
            ▼                                            │ read path (import /
     atomic sidecar write (*.xmp) ── divergence hash ────┘  ReadMetadata)
```

Failure modes owned here: torn edit txn (WAL invariant — kill-9 tested), sidecar write torn (atomic temp-rename), adversarial XMP input (fuzzed, capped), catalog/sidecar divergence (hash-tracked, never silently resolved), newer-schema doc read by older build (read-only preservation, never rewrite-and-lose).

---

## 2. Crates / modules touched

Per the architecture's decomposition (§2.1/§2.2). E09 creates or extends:

| Crate | E09 work | Notes |
|---|---|---|
| **`lightbox-edit`** (new) | `params` (ParamId/ParamValue/ParamGroup/ParamSubset/ParamDelta), `recipe` (typed model, defaults, validation, CBOR), `history` (steps, reconstruction), `session` (EditSession/EditStore, gestures), `snapshot`, `preset` (model, store, LR import), `transfer` (copy/paste/sync), `xmp_map` (crs:/lb: mapping, `to_xmp`/`from_lr_crs`) | The epic's center of gravity |
| **`lightbox-meta`** (new — XMP subsystem only) | `xmp::doc` (XmpDoc wrapper over the ISO toolkit), `xmp::sidecar` (paths, atomic I/O, embedded read), `xmp::sync` (divergence), `xmp::library_map` (rating/label/keyword/IPTC mapping) | EXIF half of the crate is E04/E07 territory |
| **`lightbox-catalog`** (extend) | migration `000N_edit_state.sql`; DAOs for `edit_recipe`, `edit_index`, `history_step`, `snapshot`, `xmp_sync`; virtual-copy recipe-duplication hook | All writes via the E01 single-writer handle |
| **`lightbox-core`** (extend) | `EditCommand`/`EditQuery` variants on the command bus; session registry (one live `EditSession` per image); wiring to the transaction/history manager | No UI types (headless boundary) |
| **`lightbox-cli`** (extend) | `edit set/history/snapshot/preset/xmp read/xmp write` subcommands for headless E2E | Drives the §8 integration tests |
| **`xmp_toolkit` dependency** | Adobe-published Rust bindings crate (MIT/Apache-2.0) vendoring the ISO 16684 XMP Toolkit C++ (BSD-3), static link | Requires CTO sign-off entry + cargo-deny + SBOM surface-2 registration (T11). Fallback: direct FFI vendoring; reversal fallback per §1.6: own quick-xml RDF behind the same `XmpDoc` API |

New third-party deps: `xmp_toolkit` (MIT/Apache wrapper, BSD-3 native), `ciborium` (MIT, CBOR), `uuid` (MIT/Apache), `directories` (MIT/Apache, preset dir), `notify` (CC0/Artistic-2.0 — verify; else `notify-debouncer` alternatives) for preset-dir watch, `proptest`/`cargo-fuzz` (dev). All pass surface-1 allowlist; the C++ toolkit is registered on surface 2.

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
    // base profile / process
    BaseProfile = 1,
    // white balance
    WhiteBalance = 10,
    // tone (basic panel)
    Exposure = 20, Contrast = 21, Highlights = 22, Shadows = 23, Whites = 24, Blacks = 25,
    // curves
    ToneCurve = 40,                    // composite + r/g/b as one value (edited as a unit)
    // color
    Hsl = 50, ColorGrade = 51, BwMix = 52, Vibrance = 53, Saturation = 54, Treatment = 55,
    // presence
    Clarity = 60, Texture = 61, Dehaze = 62,
    // detail
    Sharpen = 70, NoiseReduction = 71,
    // optics
    LensProfile = 80, ChromaticAberration = 81, Defringe = 82, VignetteCorr = 83,
    // geometry
    Crop = 90, Angle = 91, Flip = 92, Upright = 93, Transform = 94,
    // effects
    PostCropVignette = 100, Grain = 101, CreativeLut = 102,
    // structure (list-valued; content owned elsewhere — §3.1 arch)
    MaskList = 120, RetouchList = 121,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ParamValue {
    F32(f32), I32(i32), Bool(bool),
    Wb(WhiteBalance), Curve(ToneCurveSet), Hsl(HslTable), ColorGrade(ColorGrade),
    BwMix(BwMix), Sharpen(Sharpen), Nr(NoiseReduction), Profile(ProfileRef),
    Crop(Crop), Upright(Upright), Transform(Transform), Lut(CreativeLut),
    Vignette(PostCropVignette), Grain(Grain), Lens(LensCorrection),
    Ids(Vec<u64>),                     // MaskList / RetouchList
}

/// Group taxonomy for preset checklists & copy/paste dialogs (research 02: partial presets).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ParamGroup {
    BaseProfile, WhiteBalance, Tone, Curve, ColorMixer, ColorGrading, BwMix,
    Presence, Detail, Optics, Geometry, Effects, Masks, Retouch,
}
pub fn group_of(id: ParamId) -> ParamGroup;

/// A checked set of groups (with future per-param overrides).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ParamSubset { pub groups: BTreeSet<ParamGroup> }

/// The universal currency: an ordered map of changed params → new values.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct ParamDelta(pub BTreeMap<ParamId, ParamValue>);

impl ParamDelta {
    pub fn merge(&mut self, later: ParamDelta);          // later wins per key
    pub fn restrict(&self, subset: &ParamSubset) -> ParamDelta;
    pub fn is_empty(&self) -> bool;
}
```

Laws (property-tested, T4): `recipe.apply(&recipe.diff(&other))` ⇒ `recipe == other`; `apply` is idempotent per delta; `diff(a, a).is_empty()`.

### 3.2 Recipe (`lightbox_edit::recipe`) — the §3.2 schema, typed

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ProcessVersion(pub u16);          // PV1 at M1; registry semantics are E05's
pub const RECIPE_SCHEMA: u16 = 1;            // serialization schema, independent of PV

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Recipe {
    pub schema: u16,
    pub process_version: ProcessVersion,     // immutable per image (§4.5)
    pub base_profile: ProfileRef,
    pub global: GlobalStages,
    pub geometry: Geometry,
    pub masks: Vec<MaskId>,                  // ordered refs only (content: E12 tables)
    pub retouch: Vec<RetouchOpId>,           // ordered refs only (content: E12 tables)
    pub lb_extra: BTreeMap<String, CborValue>,       // lb:-namespace extension bag
    pub xmp_passthrough: XmpPassthrough,     // foreign fields, preserved verbatim
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown: BTreeMap<String, CborValue>,        // forward-compat: keys a newer build wrote
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ProfileRef {
    pub kind: ProfileKind,                   // Matrix (license-clean default, §1.7) | Dcp
    pub id: String,                          // "matrix-base" | curated/user profile id
    pub look_ref: Option<String>,            // Lightbox default look family (§1.7 tier 2)
    pub look_amount: f32,                    // 0.0..=2.0, default 1.0
}

/// Full §3.2 global set, typed NOW so E10/E11 add no schema fields.
/// All defaults are neutral (identity render).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct GlobalStages {
    pub white_balance: WhiteBalance,         // AsShot | Auto | Preset(WbPreset) | Custom{temp_k, tint}
    pub exposure: f32,                       // stops, -5.0..=5.0
    pub contrast: f32, pub highlights: f32, pub shadows: f32,   // -100..=100
    pub whites: f32, pub blacks: f32,                            // -100..=100
    pub tone_curve: ToneCurveSet,            // rgb + r/g/b point curves, normalized [0,1], ≤64 pts, monotone-x
    pub hsl: HslTable,                       // [HslBand; 8]: hue/sat/lum, -100..=100
    pub color_grade: ColorGrade,             // shadow/mid/high/global wheels + blend + balance
    pub treatment: Treatment,                // Color | BlackAndWhite
    pub bw: BwMix,                           // [f32; 8]
    pub vibrance: f32, pub saturation: f32,
    pub presence: Presence,                  // clarity, texture, dehaze
    pub detail: Detail,                      // Sharpen{amount,radius,detail,masking}, Nr{luma,luma_detail,chroma,chroma_detail}
    pub optics: Optics,                      // lens_profile: Option<LensCorrection>, ca: bool, defringe, vignette_corr
    pub effects: Effects,                    // postcrop_vignette, grain, creative_lut: Option<CreativeLut>
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Geometry { pub crop: Crop, pub angle: f32, pub flip: Flip,
                      pub upright: Upright, pub transform: Transform }

impl Recipe {
    /// Neutral recipe for a probed asset (as-shot WB, matrix base profile, full crop).
    pub fn default_for(pv: ProcessVersion, probe: &AssetProbe) -> Recipe;
    pub fn apply(&mut self, delta: &ParamDelta) -> Result<(), RecipeError>;   // validates+clamps
    pub fn diff(&self, from: &Recipe) -> ParamDelta;
    pub fn extract(&self, subset: &ParamSubset) -> ParamDelta;
    pub fn get(&self, id: ParamId) -> ParamValue;
    pub fn is_neutral(&self) -> bool;                                          // drives edit_index.is_edited

    // serialization (T3)
    pub fn to_cbor(&self) -> Vec<u8>;
    pub fn from_cbor(bytes: &[u8]) -> Result<RecipeRead, RecipeError>;
    /// Deterministic encoding for hashing/divergence (struct-order fields, sorted maps).
    pub fn canonical_hash(&self) -> [u8; 16];                                  // xxh3-128
}

/// Forward-compat read result: a doc written by a newer build is preserved, not rewritten.
pub enum RecipeRead {
    Ok(Recipe),
    /// schema > RECIPE_SCHEMA: render best-effort from known fields is NOT attempted at M1;
    /// the doc is locked read-only and surfaced to the shell as "edited in a newer Lightbox".
    NewerSchema { raw: Vec<u8>, schema: u16 },
}
```

### 3.3 Persistence & session (`lightbox_edit::{session, history, snapshot}`)

```rust
pub struct EditStore { /* catalog reader pool + writer handle (E01) */ }
impl EditStore {
    pub fn new(cat: Arc<Catalog>) -> EditStore;
    /// Loads recipe or creates the neutral default row lazily (first develop-open / import).
    pub fn load(&self, image: ImageId) -> Result<EditState>;
    pub fn open_session(&self, image: ImageId) -> Result<EditSession>;  // one per image, enforced by core registry
    /// Virtual-copy hook (E07 calls via core): copy doc, fresh history with one "Create Virtual Copy" step.
    pub fn clone_for_virtual_copy(&self, src: ImageId, dst: ImageId) -> Result<()>;
    pub fn recipe_of(&self, image: ImageId) -> Result<RecipeRead>;      // read-only, WAL reader
}

pub struct EditState { pub recipe: Recipe, pub head_seq: u64, pub updated_at: Timestamp }

/// In-memory working state; the render engine reads `working()` — never the DB — during drags.
pub struct EditSession { /* image, working: Recipe, committed: Recipe, gesture: Option<Gesture> */ }
impl EditSession {
    pub fn working(&self) -> &Recipe;
    /// Gesture = one history step (slider drag, one preset apply, one paste).
    pub fn begin_gesture(&mut self, label: StepLabel);
    pub fn update(&mut self, delta: ParamDelta);                 // in-memory only; coalesced
    /// One WAL txn: truncate history_step > head_seq, append step (delta+inverse),
    /// update edit_recipe.doc + head_seq, rebuild edit_index. p95 < 5 ms.
    pub fn commit_gesture(&mut self) -> Result<HistoryStepId>;
    pub fn cancel_gesture(&mut self);

    pub fn apply_preset(&mut self, p: &DevelopPreset) -> Result<HistoryStepId>;
    pub fn paste(&mut self, s: &CopiedSettings) -> Result<HistoryStepId>;
    pub fn reset(&mut self) -> Result<HistoryStepId>;            // back to default_for(pv)

    // history navigation (persistent, Lightroom semantics)
    pub fn step_to(&mut self, step: HistoryStepId) -> Result<()>;    // sets working+doc+head_seq; keeps later steps until next commit truncates
    pub fn create_snapshot(&mut self, name: &str) -> Result<SnapshotId>;
    pub fn restore_snapshot(&mut self, id: SnapshotId) -> Result<HistoryStepId>; // restore is itself a step
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StepLabel {
    Param(ParamId),                       // "Exposure"
    Preset { name: String },
    Paste, Reset, ImportDefault, CrsImport, VirtualCopy,
    SnapshotRestore { name: String }, HistoryRestore { seq: u64 },
}

pub struct HistoryStepMeta { pub id: HistoryStepId, pub seq: u64, pub label: StepLabel,
                             pub ts: Timestamp, pub is_head: bool }

pub mod history {
    /// Newest-first for the panel. Cheap: metadata only.
    pub fn list(cat: &Catalog, image: ImageId) -> Result<Vec<HistoryStepMeta>>;
    /// Reconstruct state at a step: nearest keyframe (every 64 steps) + delta replay. <10 ms @ 1k steps.
    pub fn recipe_at(cat: &Catalog, image: ImageId, seq: u64) -> Result<Recipe>;
    pub fn clear(cat: &Catalog, image: ImageId) -> Result<()>;   // keeps current doc; drops steps
}

pub struct Snapshot { pub id: SnapshotId, pub name: String, pub ts: Timestamp,
                      pub recipe_doc: Vec<u8> /* self-contained materialized projection */ }
/// E12 seam: snapshots must be self-contained (§3.1 — snapshot is a copy, not a live owner).
/// At M1 mask/retouch lists are empty; E12 registers a materializer that inlines content.
pub trait SnapshotMaterializer: Send + Sync {
    fn materialize(&self, r: &Recipe) -> Result<CborValue>;      // content for masks/retouch refs
    fn restore(&self, doc: &CborValue, image: ImageId) -> Result<Recipe>;
}
```

### 3.4 Core command/query surface (`lightbox-core`, headless boundary)

```rust
pub enum EditCommand {
    OpenSession { image: ImageId },
    CloseSession { image: ImageId },
    Gesture { image: ImageId, label: StepLabel, delta: ParamDelta, phase: GesturePhase }, // Begin|Update|Commit|Cancel
    ApplyPreset { images: Vec<ImageId>, preset: PresetId },      // multi-target = batch, one step per image
    CopySettings { image: ImageId, subset: ParamSubset },
    PasteSettings { images: Vec<ImageId> },
    SyncSettings { source: ImageId, targets: Vec<ImageId>, subset: ParamSubset },
    ApplyPrevious { images: Vec<ImageId> },
    StepTo { image: ImageId, seq: u64 },
    ClearHistory { image: ImageId },
    CreateSnapshot { image: ImageId, name: String },
    RestoreSnapshot { image: ImageId, snapshot: SnapshotId },
    DeleteSnapshot { image: ImageId, snapshot: SnapshotId },
    RenameSnapshot { image: ImageId, snapshot: SnapshotId, name: String },
    ResetEdits { images: Vec<ImageId> },
    // XMP
    ReadMetadata { assets: Vec<AssetId>, what: MetadataScope },  // Develop | Library | All
    WriteMetadata { assets: Vec<AssetId>, what: MetadataScope },
    SetAutoWriteXmp { enabled: bool },
    // presets
    CreatePreset { from: ImageId, name: String, group: Option<String>, subset: ParamSubset },
    ImportPresetFiles { paths: Vec<PathBuf> },                   // LR or Lightbox .xmp
    DeletePreset { preset: PresetId }, RenamePreset { preset: PresetId, name: String },
    ExportPreset { preset: PresetId, dest: PathBuf },
}

pub enum EditQuery {
    WorkingRecipe { image: ImageId },            // -> Recipe (render seam, E05/E10)
    History { image: ImageId },                  // -> Vec<HistoryStepMeta>
    Snapshots { image: ImageId },
    Presets,                                     // -> preset tree (groups + metas)
    PresetPreviewRecipe { image: ImageId, preset: PresetId },    // pure; hover preview (E08)
    XmpDivergence { assets: Vec<AssetId> },      // -> Vec<(AssetId, DivergenceStatus)> (E07 badge)
    CrsImportReport { image: ImageId },          // fidelity report from last import
}
```

App-level undo/redo (Cmd-Z) is E01's transaction/history manager; it maps a committed develop gesture to an undo entry that calls `StepTo{seq-1}`/`StepTo{seq}` — E09 exposes the seq handles, E01 owns the stack.

### 3.5 XMP substrate (`lightbox_meta::xmp`)

```rust
/// Thin, swappable wrapper over the ISO 16684 XMP Toolkit (via the Adobe-published
/// `xmp_toolkit` Rust bindings). §1.6 reversal fallback (own quick-xml RDF) re-implements
/// exactly this API — nothing above it may import toolkit types.
pub struct XmpDoc(/* xmp_toolkit::XmpMeta */);

pub mod ns {
    pub const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
    pub const LB:  &str = "http://lightbox.app/ns/1.0/";         // registered prefix "lb"
    pub const XMP: &str = "http://ns.adobe.com/xap/1.0/";
    pub const DC:  &str = "http://purl.org/dc/elements/1.1/";
    pub const LR:  &str = "http://ns.adobe.com/lightroom/1.0/";
    // + photoshop:, Iptc4xmpCore: as needed by library mapping
}

impl XmpDoc {
    pub fn new() -> XmpDoc;
    pub fn parse(packet: &[u8], limits: ParseLimits) -> Result<XmpDoc, XmpError>; // size/depth caps (T14)
    pub fn serialize(&self) -> Result<String, XmpError>;          // canonical RDF/XML packet

    pub fn get(&self, ns: &str, path: &str) -> Option<XmpValue>;
    pub fn set(&mut self, ns: &str, path: &str, v: XmpValue) -> Result<(), XmpError>;
    pub fn delete(&mut self, ns: &str, path: &str);
    pub fn get_array(&self, ns: &str, name: &str) -> Option<Vec<XmpValue>>;     // Seq/Bag/Alt
    pub fn set_array(&mut self, ns: &str, name: &str, kind: ArrayKind, items: &[XmpValue]) -> Result<(), XmpError>;
    // struct fields addressed by path, e.g. "crs:ToneCurvePV2012[3]"
}

pub mod sidecar {
    /// `IMG_1234.CR3` → `IMG_1234.xmp` (LR-compatible naming; collision policy documented).
    pub fn sidecar_path(original: &Path) -> PathBuf;
    pub fn read(path: &Path) -> Result<Option<XmpDoc>>;
    /// Atomic: temp file + fsync + rename. NEVER writes into `original`.
    pub fn write_atomic(path: &Path, doc: &XmpDoc) -> Result<SidecarStamp>;     // returns hash+mtime
    /// Read-only embedded packet extraction (JPEG/TIFF/DNG/HEIC) via XMPFiles handlers,
    /// with a bounded brute-force packet scan fallback for unhandled containers.
    pub fn read_embedded(original: &Path) -> Result<Option<XmpDoc>>;
}

pub mod sync {
    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum DivergenceStatus { NoSidecar, InSync, CatalogNewer, SidecarNewer, Conflict }
    /// Compares stored SidecarStamp (xmp_sync row) against on-disk state + catalog updated_at.
    pub fn status(cat: &Catalog, asset: AssetId) -> Result<DivergenceStatus>;
    /// fs-watcher (E07) calls this on *.xmp events; updates xmp_sync.divergent, emits core event.
    pub fn note_fs_event(cat: &Catalog, path: &Path) -> Result<()>;
}
```

### 3.6 Mapping layer (`lightbox_edit::xmp_map`)

```rust
pub struct XmpWriteCtx<'a> {
    pub library: Option<&'a LibraryFields>,   // rating/label/keywords/title/caption/IPTC (from E07 tables)
    pub app_version: &'a str,                 // provenance: lb:CreatorTool-ish
}

impl Recipe {
    /// Projection (read-only materialization, §3.1): full recipe → lb: namespace (authoritative,
    /// CBOR-faithful), PLUS best-effort crs: compatibility fields for LR readers, PLUS
    /// xmp_passthrough foreign fields re-emitted verbatim, PLUS library fields if provided.
    pub fn to_xmp(&self, ctx: &XmpWriteCtx) -> Result<XmpDoc, XmpMapError>;

    /// Read back our own sidecar: lb: primary; if absent, falls back to crs: (≈ LR import).
    pub fn read_xmp(doc: &XmpDoc, probe: &AssetProbe) -> Result<RecipeFromXmp, XmpMapError>;

    /// One-way Lightroom migration (Risk 9): crs: → Recipe + per-field fidelity report.
    /// Never fails on unknown fields — they land in xmp_passthrough + report.skipped.
    pub fn from_lr_crs(doc: &XmpDoc, probe: &AssetProbe) -> CrsImport;
}

pub struct CrsImport { pub recipe: Recipe, pub report: CrsImportReport }
#[derive(Serialize)]
pub struct CrsImportReport {
    pub mapped: Vec<FieldFidelity>,        // 1:1 numeric mappings
    pub approximate: Vec<FieldFidelity>,   // domain-translated (tone curve, PV<3 legacy params, split-toning→grade)
    pub skipped: Vec<String>,              // e.g. crs:MaskGroupBasedCorrections (preserved in passthrough)
    pub source_pv: Option<String>,         // crs:ProcessVersion as written by LR
}

/// Library metadata mapping (used by Read/WriteMetadata; catalog rows are E07's).
pub mod library_map {
    pub fn to_xmp(fields: &LibraryFields, doc: &mut XmpDoc) -> Result<(), XmpMapError>;
    pub fn from_xmp(doc: &XmpDoc) -> LibraryFields;   // xmp:Rating, xmp:Label, dc:subject,
                                                      // lr:hierarchicalSubject ("A|B|C"), dc:title,
                                                      // dc:description, IPTC Core creator/copyright/location
}
```

The **documented mapping table** ships as `docs/interop/crs-mapping.md` (T16): one row per field — Lightbox param, `crs:` property, value domain both sides, conversion, fidelity class (exact / approximate / skipped), notes. This file is the normative reference the property tests are generated from, and the deliverable Risk 9's "documented mapping" language requires.

### 3.7 Presets (`lightbox_edit::preset`)

```rust
#[derive(Clone, Debug)]
pub struct DevelopPreset {
    pub id: PresetId,                       // uuid v4, stable across renames
    pub name: String,
    pub group: Option<String>,              // folder-derived
    pub subset: ParamSubset,                // which groups the preset carries
    pub delta: ParamDelta,                  // partial values (only carried params)
    pub min_pv: ProcessVersion,
    pub origin: PresetOrigin,               // Lightbox | LightroomImport { source_pv: String }
    pub path: PathBuf,                      // canonical .xmp file
}

/// DECISION (in-epic authority; architecture is silent): presets are app-level assets, not
/// catalog rows. Canonical store = one .xmp file per preset under the platform config dir
/// (`<config>/Lightbox/presets/<group>/<name>.xmp`), Lightroom-compatible on disk, so
/// LR presets import by file drop and users share presets as files. The registry is an
/// in-memory index kept fresh by a directory watcher. Rationale: presets outlive any single
/// catalog; the file format IS our to_xmp mapping (dogfood); zero new catalog tables.
/// Reversal trigger: if per-catalog preset scoping or sync-ordering demands arise, add a
/// catalog-side index table over the same files — the PresetStore API below doesn't change.
pub struct PresetStore { /* root dir, watcher, index */ }
impl PresetStore {
    pub fn open(root: PathBuf) -> Result<PresetStore>;
    pub fn list(&self) -> Vec<PresetMeta>;                        // grouped, sorted
    pub fn get(&self, id: PresetId) -> Option<Arc<DevelopPreset>>;
    pub fn create_from(&self, recipe: &Recipe, name: &str, group: Option<&str>,
                       subset: &ParamSubset) -> Result<DevelopPreset>;
    pub fn import_files(&self, paths: &[PathBuf]) -> Vec<PresetImportResult>;  // LR or Lightbox .xmp
    pub fn rename(&self, id: PresetId, name: &str) -> Result<()>;
    pub fn delete(&self, id: PresetId) -> Result<()>;             // moves file to trash
    pub fn export(&self, id: PresetId, dest: &Path) -> Result<()>;
}

/// Pure — no session, no commit. E08 hover preview and E04 apply-on-import both use this.
pub fn preview_recipe(base: &Recipe, preset: &DevelopPreset) -> Recipe;
```

### 3.8 Settings transfer (`lightbox_edit::transfer`)

```rust
pub struct CopiedSettings { pub subset: ParamSubset, pub delta: ParamDelta,
                            pub source_pv: ProcessVersion }
/// Sync engine: applies one delta to N images. Batched: chunks of ~64 images per WAL txn
/// (§7 burst-write mitigation), one history_step per image, edit_index rebuilt per row.
/// Emits per-image progress events; cancellable via E06 CancelToken when run as a job.
pub fn sync_to(cat: &Catalog, delta: &ParamDelta, targets: &[ImageId],
               label: StepLabel, cancel: &CancelToken) -> Result<SyncReport>;
```

---

## 4. Data model — migration `000N_edit_state.sql`

Owned by `lightbox-catalog`, applied through E01's migration framework (copy-on-write upgrade per §6). Refines §3.1's `history_step (op, params)` into `(op, delta, inverse, keyframe_doc)` — a planner-level refinement enabling O(distance) history stepping and O(1) undo, called out here explicitly.

```sql
-- E09: edit state, history, snapshots, xmp sync
CREATE TABLE edit_recipe (
    image_id    INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
    pv          INTEGER NOT NULL,                 -- ProcessVersion, immutable (§4.5)
    schema      INTEGER NOT NULL,                 -- RECIPE_SCHEMA at write time
    doc         BLOB    NOT NULL,                 -- CBOR Recipe (authoritative, §3.2)
    head_seq    INTEGER NOT NULL DEFAULT 0,       -- history position doc corresponds to
    updated_at  INTEGER NOT NULL                  -- unix ms
);

CREATE TABLE edit_index (                         -- derived; rebuilt in the SAME txn as doc (§3.1)
    image_id     INTEGER PRIMARY KEY REFERENCES image(id) ON DELETE CASCADE,
    is_edited    INTEGER NOT NULL DEFAULT 0,
    has_masks    INTEGER NOT NULL DEFAULT 0,      -- E12 populates; column reserved now
    has_ai_mask  INTEGER NOT NULL DEFAULT 0,      -- E14 populates; column reserved now
    crop_ratio   REAL,
    treatment    TEXT,                            -- 'color' | 'bw' (filter facet)
    base_profile TEXT,                            -- profile id (filter facet)
    updated_at   INTEGER NOT NULL
);
CREATE INDEX idx_edit_index_edited ON edit_index(is_edited);

CREATE TABLE history_step (
    id           INTEGER PRIMARY KEY,
    image_id     INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,                -- 1..n, dense per image
    op           BLOB    NOT NULL,                -- CBOR StepLabel
    delta        BLOB    NOT NULL,                -- CBOR ParamDelta (forward)
    inverse      BLOB    NOT NULL,                -- CBOR ParamDelta (backward)
    keyframe_doc BLOB,                            -- full CBOR Recipe every 64th step, else NULL
    ts           INTEGER NOT NULL,
    UNIQUE(image_id, seq)
);

CREATE TABLE snapshot (
    id         INTEGER PRIMARY KEY,
    image_id   INTEGER NOT NULL REFERENCES image(id) ON DELETE CASCADE,
    name       TEXT    NOT NULL,
    recipe_doc BLOB    NOT NULL,                  -- SELF-CONTAINED materialized projection (§3.1)
    ts         INTEGER NOT NULL,
    UNIQUE(image_id, name)
);

CREATE TABLE xmp_sync (                           -- per-asset sidecar bookkeeping (§6 divergence row)
    asset_id        INTEGER PRIMARY KEY REFERENCES asset(id) ON DELETE CASCADE,
    sidecar_hash    BLOB,                         -- xxh3-128 of last known sidecar bytes
    sidecar_mtime   INTEGER,
    last_written_at INTEGER,                      -- by us
    last_read_at    INTEGER,                      -- into catalog
    divergent       INTEGER NOT NULL DEFAULT 0
);
```

Write-path invariants (enforced in `EditStore`/DAO, fault-injection tested):

1. `edit_recipe.doc`, `edit_index`, and `history_step` mutate **only together, in one WAL txn**, through the single writer (E01).
2. Commit protocol: `DELETE FROM history_step WHERE image_id=? AND seq > head_seq` → `INSERT` new step (`seq = head_seq+1`, keyframe every 64) → `UPDATE edit_recipe SET doc, head_seq, schema, updated_at` → rebuild `edit_index` row. Lightroom truncation semantics fall out of `head_seq`.
3. `pv` never changes in an UPDATE (guarded by a DAO assertion; the migrate command is a v1.x door).
4. Nothing outside `lightbox-catalog` issues SQL (headless boundary; no SQL crosses up).
5. Auto-write-XMP and preset-store state are **not** catalog data: auto-write flag lives in app prefs (E08 prefs store); presets live on disk (§3.7 decision).

Storage estimate: doc ≈ 1–3 KB; step rows ≈ 100–400 B + 1 keyframe/64 steps. A 100k-image catalog with 50 steps/image ≈ ~2.5 GB worst-case ceiling → acceptable, plus `ClearHistory` and a (default-off) history cap preference named in Open Questions.

---

## 5. Ordered task breakdown (each ≤1 day)

**First failing test of the epic (per §12 staff-engineer bar):** `recipe_default_roundtrip` — `Recipe::default_for(PV1, probe)` → `to_cbor` → `from_cbor` yields an equal `Recipe`, and `canonical_hash` is byte-identical on macOS/Windows/Linux CI. Written in T1–T3, red until T3 lands.

### Phase A — recipe model & serialization (`lightbox-edit`) — 4 days

| # | Task | Acceptance criteria |
|---|---|---|
| T1 | Crate scaffold + param vocabulary: `ParamId` (stable u16 wire ids), `ParamValue`, `ParamGroup`, `group_of`, `ParamSubset`, `ParamDelta` merge/restrict. | Wire-id snapshot test fails on any renumbering; every `ParamId` maps to exactly one group (exhaustive match, no wildcard). |
| T2 | Typed `Recipe`/`GlobalStages`/`Geometry`/`ProfileRef` + all §3.2 leaf types with neutral defaults, `default_for`, validation/clamping (ranges above; tone-curve monotone-x, ≤64 pts). | `default_for(PV1).is_neutral()`; out-of-range `apply` clamps and reports `RecipeError::Clamped`; curve invariants property-tested. |
| T3 | CBOR serialization (`ciborium`): struct-order deterministic encode, `unknown` map preservation, `RECIPE_SCHEMA` gate → `RecipeRead::NewerSchema` (read-only, never rewritten). | First failing test green; proptest encode/decode identity over arbitrary recipes; doc with injected unknown keys round-trips byte-preserving; newer-schema doc read+rewrite leaves original bytes untouched. |
| T4 | Delta engine: `Recipe::{apply,diff,extract,get}`, `canonical_hash` (xxh3-128 over canonical CBOR). | Proptest laws: `a.apply(b.diff(a)) == b`; `diff(a,a)` empty; `extract(subset)` ⊆ subset's groups; hash identical across 3-OS CI. |

### Phase B — persistence, history, session — 6 days

| # | Task | Acceptance criteria |
|---|---|---|
| T5 | Migration `000N_edit_state.sql` + DAO skeleton (typed row structs, prepared statements) in `lightbox-catalog`. | Migration applies to an E01 fixture catalog and to an empty catalog; FK cascades verified; E01 kill-9 fault-injection suite extended over an edit-write loop stays `integrity_check`-clean. |
| T6 | `EditStore`: `load` (lazy neutral-default row creation), `recipe_of`, `edit_index` rebuild fn, `clone_for_virtual_copy` (copy doc; fresh history seeded with one `VirtualCopy` step). | Loading an untouched image creates no row until first commit; index row always consistent with doc in the same txn (assert via test hook); VC of an edited image renders-equal recipe (struct equality) with `head_seq == 1`. |
| T7 | `EditSession` gesture lifecycle + commit protocol (§4 invariant 2), core session registry (one live session per image). | Slider-drag simulation (500 `update`s, 1 commit) writes exactly 1 step; commit p95 < 5 ms with 1k existing steps (criterion, mid-range SSD); `working()` reads never touch the DB (assert with a poisoned-connection test double). |
| T8 | History reconstruction & navigation: keyframe-every-64, `recipe_at` (nearest anchor + replay), `step_to` (persists doc+head_seq), truncation-on-commit-after-step-back, `clear`, `ImportDefault` initial step. | `recipe_at` over a 1,000-step fixture < 10 ms; property test: for random step sequences, `recipe_at(k)` equals brute-force replay; step-back-then-edit drops later steps exactly (LR semantics). |
| T9 | Snapshots: CRUD, `restore_snapshot` (a new history step), promote-history-step-to-snapshot, `SnapshotMaterializer` registry (M1 default materializer = identity; E12 seam). | Create→mutate→restore yields struct-equal recipe; snapshot doc unchanged by later edits; name uniqueness per image enforced (`UNIQUE` surfaced as a typed error). |
| T10 | `lightbox-core` wiring: `EditCommand`/`EditQuery` variants, session registry, events (`EditCommitted{image, seq}` for E05 re-render + E15/E07 listeners), `lightbox-cli edit` subcommands. | Headless CLI script: open → set exposure → history list → step-to → snapshot → restore, all green; command bus rejects a second concurrent session on the same image with a typed error. |

### Phase C — XMP substrate (`lightbox-meta`) — 5 days

| # | Task | Acceptance criteria |
|---|---|---|
| T11 | Integrate `xmp_toolkit` (vendored ISO toolkit build): CI on macOS/Win/Linux; license plumbing — cargo-deny allowlist entry, SBOM surface-2 artifact entry (static BSD-3 + wrapper MIT/Apache), CTO sign-off checklist item filed; `XmpDoc::{new,parse,serialize}`. | 3-OS CI builds; parse→serialize on an LR-Classic sidecar fixture preserves all foreign properties (semantic diff empty); license CI surfaces green; sign-off ticket linked in the PR. |
| T12 | Typed property access: namespaces (`crs`/`lb`/`xmp`/`dc`/`lr`/`photoshop`/`Iptc4xmpCore`), scalars, Seq/Bag/Alt arrays, struct paths (`crs:ToneCurvePV2012[3]`), `lb:` prefix registration. | Unit tests: read `crs:ToneCurvePV2012` array + `lr:hierarchicalSubject` bag from fixtures; write/read-back typed values of every `XmpValue` kind. |
| T13 | Sidecar + embedded I/O: `sidecar_path` (LR-compatible `<stem>.xmp`; multi-extension collision policy documented), atomic `write_atomic` (temp+fsync+rename), `read`, `read_embedded` (XMPFiles read-only: JPEG/TIFF/DNG/HEIC; bounded packet-scan fallback). | Fixtures per container format; a test asserting the original file's bytes+mtime are untouched by every write path (mandate constraint 2 guard); torn-write simulation leaves either old or new sidecar, never a partial. |
| T14 | Untrusted-input hardening: `ParseLimits` (packet ≤ 16 MB, depth/property caps, UTF-8 validation), `cargo-fuzz` target over `XmpDoc::parse` + packet-scan, adversarial corpus (truncated/recursive/entity-bomb), CI fuzz smoke (≥1 h nightly, 5 min PR). | Zero crashes/hangs/OOM on corpus + fuzz smoke; over-limit inputs return typed errors; findings triage doc committed (feeds the §12 security review). |
| T15 | Divergence tracking: `xmp_sync` DAO, `SidecarStamp` hashing, `sync::status`, `note_fs_event` (E07 watcher seam), core event `XmpDivergenceChanged`. | Externally rewriting a sidecar flips status to `SidecarNewer`; our own `write_atomic` does not (stamp updated in same txn); batch status query for 1k assets < 10 ms. |

### Phase D — `crs:`/`lb:` mapping layer — 6 days

| # | Task | Acceptance criteria |
|---|---|---|
| T16 | Mapping table + converters: `docs/interop/crs-mapping.md` (normative, per-field: property, domains, conversion, fidelity class) + table-driven converter fns (exposure stops, ±100 sliders, tone-curve 0–255→[0,1], WB presets, split-toning→color-grade, PV string→fidelity note). | Doc covers every §3.2 global/geometry field; converter unit tests generated from the table; unmapped-by-design fields (masks/retouch) listed with rationale. |
| T17 | `Recipe::to_xmp`: `lb:` full-fidelity emit (schema-versioned, CBOR-faithful), `crs:` compatibility emit for mappable fields, `xmp_passthrough` re-emission, provenance (`lb:Schema`, `lb:ProcessVersion`, app version), library fields via `XmpWriteCtx`. | Emitted packet parses in exiftool (subprocess, dev-only) without warnings; contains `crs:Exposure2012` etc. per mapping table; foreign fixture fields present verbatim; snapshot test of full packet. |
| T18 | `Recipe::read_xmp` (lb: primary, crs: fallback) + round-trip identity. | Proptest: arbitrary recipe → `to_xmp` → `read_xmp` → struct-equal recipe (lb: path); crs:-only doc falls back to the T19 import path; `xmp_passthrough` populated with everything not ours. |
| T19 | `from_lr_crs` one-way import + `CrsImportReport`; LR sidecar corpus (PV3/4/5/6, LR Classic ≥ 7 era) as fixtures; legacy-param handling (pre-2012 Exposure/Brightness/Fill → approximate, reported). | Corpus assertions per file (expected mapped/approximate/skipped sets); `crs:MaskGroupBasedCorrections` → skipped + preserved; import never errors on unknown fields (proptest with injected junk crs: props); report serializes for the E16/UI seam. |
| T20 | `library_map`: rating/label/keywords (hierarchy `A|B|C`), title/caption, IPTC Core creator/copyright/location ↔ `XmpDoc`, as pure functions over E07's row types. | Round-trip property tests incl. hierarchical keywords with pipes-in-names escaping policy documented; label color name sets (LR default set) map both ways. |
| T21 | `ReadMetadata`/`WriteMetadata` commands (single + batch): conflict policy (catalog is source of truth; read overwrites catalog only on explicit command; write overwrites sidecar always, stamping `xmp_sync`), opt-in auto-write (pref-gated, debounced ≥2 s/image, enqueued as E06 Background job; graceful no-op if job system absent in tests). | CLI: edit → `xmp write` → fresh catalog → `xmp read` → recipes struct-equal; divergent sidecar is never silently read (badge until explicit command); auto-write coalesces a 50-commit burst into ≤2 writes. |

### Phase E — presets & settings transfer — 6 days

| # | Task | Acceptance criteria |
|---|---|---|
| T22 | `PresetStore`: platform config dir layout, group-as-folder, uuid sidecar property (`lb:PresetId`), index + directory watcher, cold-scan. | Cold scan of 500 preset files < 100 ms; dropping a valid `.xmp` into the folder surfaces it via query event within 1 s; malformed file → quarantined with typed error, store stays up. |
| T23 | Create/export presets: `create_from(recipe, subset)` writing partial `.xmp` (only checked groups' fields), rename/delete (to OS trash), export-to-path. | Created preset applied to a neutral recipe changes exactly the checked groups (property test over random subsets); exported file re-imports to an equal preset. |
| T24 | Apply paths as history steps: `apply_preset` (one step, `StepLabel::Preset`), `paste`, `ApplyPrevious`; pure `preview_recipe` for hover (no session mutation); apply-on-import entry point for E04 (`preset::apply_to_default`). | Apply → single history step labelled with preset name; `step_to(seq-1)` restores exactly; `preview_recipe` has no observable side effects (checked via store instrumentation). |
| T25 | LR `.xmp` preset import: partial-field detection (present crs: keys define the subset), PV normalization, per-file `PresetImportResult` report; fixture corpus of representative freely-licensed LR presets. | Each corpus preset imports with the expected group subset; unmappable fields → report + dropped (presets carry no passthrough); a Lightbox-exported preset imports via the same path unchanged. |
| T26 | Copy/sync engine: `CopiedSettings` buffer (core-held), `SyncSettings` batched txns (~64 images/txn), one step per target, progress events, `CancelToken` honored between chunks. | Sync of a 1,000-image selection completes < 1 s (criterion, warm catalog) and yields 1,000 individual history steps; cancel mid-way leaves completed chunks committed, rest untouched, catalog consistent. |
| T27 | E2E + gates + polish: `lightbox-cli` scenario (import fixtures → apply LR preset → gestures → history walk → snapshot → write sidecars → open fresh catalog → read sidecars → assert equality + divergence states); wire criterion benches into nightly perf harness; rustdoc pass on public APIs; close out crs-mapping doc review. | Scenario green on 3-OS CI (PR-blocking fast subset per §8); all perf ACs tracked in the nightly dashboard; `cargo doc` clean; DoD checklist (below) walked. |

Total: 27 tasks ≈ 27 dev-days ≈ **5.4 pw** — inside the M (~4–6 pw) budget with T14/T19/T27 as the likely spill points (each independently droppable to the epic's tail without blocking dependents).

Ordering constraints: A strictly before B/D-mapping; C is parallel to B (different crate, different engineer if two are available); T16–T19 need T11–T13; E needs T4 + T17–T19; T21 needs T15 + T20; T10 can land right after T7 with stubs for later commands.

---

## 6. Test plan

Per the architecture's §8 strategy; E09 rows marked PR-blocking unless noted.

**Unit (PR-blocking).** Param vocabulary invariants (T1); recipe defaults/validation (T2); every crs: converter against the mapping table (T16); library-map field pairs (T20); sidecar path/collision policy (T13); divergence state machine (T15); preset partial semantics (T23).

**Property tests — `proptest` (PR-blocking; §8 names these explicitly).**
- `recipe → to_cbor → from_cbor ≡ recipe` (arbitrary recipes incl. extremes).
- Unknown-field preservation: CBOR docs with injected unknown keys and XMP packets with injected foreign properties survive read→write byte/semantically intact.
- Delta laws: `apply∘diff` identity, `extract⊆subset`, merge associativity (last-wins).
- `recipe → to_xmp → read_xmp ≡ recipe` (lb: path).
- `from_lr_crs` total: never panics/errors over fuzzed crs: property soup; report partitions are exhaustive (mapped ∪ approximate ∪ skipped covers all crs: keys seen).
- History: `recipe_at(k)` ≡ brute-force replay for random gesture sequences; truncation preserves the delta/inverse chain integrity.

**Fixture corpora (PR-blocking, committed under `tests/fixtures/`).** LR Classic sidecars across PV3–PV6; LR develop presets (freely licensed); our own emitted sidecars (goldens, snapshot-tested); adversarial XMP (fuzz seeds); multi-format embedded-XMP containers (JPEG/TIFF/DNG/HEIC).

**Fuzzing (nightly ≥1 h + 5-min PR smoke).** `XmpDoc::parse`, embedded packet scan, `Recipe::from_cbor`. Zero crash/hang/OOM gate. (Feeds the §12 security-engineer review of XMP import parsing.)

**Integration / E2E (fast subset PR-blocking; full nightly).** T27 CLI scenario; kill-9 fault injection over the edit-commit loop (extends E01's harness — the §8 catalog-crash-safety gate); divergence workflow (external mutation → badge → explicit read); auto-write debounce under commit storms.

**Performance (`criterion`, nightly; regressions filed non-blocking per §8).** Commit txn p95 < 5 ms @ 1k-step history; `recipe_at` < 10 ms @ 1k steps; CBOR encode/decode < 50 µs typical recipe; 1k-image sync < 1 s; preset cold scan (500) < 100 ms; batch divergence query (1k assets) < 10 ms. Rationale: keystroke-speed culling and the <100 ms slider budget (§7) require edit writes off the render path and tiny txns.

**Golden-image tests: none in E09** (no pixels). E09's analogue is the sidecar/CBOR snapshot goldens above; render goldens over recipes are E05/E10's gate consuming our fixtures.

**Cross-platform (PR-blocking).** All of the above on the macOS/Windows/Linux matrix — the C++ toolkit build (T11) and canonical-hash determinism (T4) are the platform-sensitive spots.

---

## 7. Risks & open questions

### Risks

| # | Risk | Mitigation / owner |
|---|---|---|
| R1 | **XMP Toolkit build/integration weight** (C++/CMake under cargo, 3 OSes) exceeds the "bounded FFI-integration cost" bet (§1.6/Risk 9). | Use the Adobe-published `xmp_toolkit` bindings crate rather than hand FFI (T11); it vendors and builds the toolkit. Hard reversal named by the architecture: swap the substrate for own quick-xml RDF **behind the same `XmpDoc` API** — nothing above `lightbox_meta::xmp::doc` may import toolkit types (enforced by a lint/visibility test). Trigger: T11 slips > 3 days or any platform's build proves unmaintainable. |
| R2 | **CTO license sign-off** for the toolkit not yet on record (mandate v1.1 requires per-dependency approval). | Procedural, filed at T11 start; merge of T11 is gated on the recorded sign-off. Fallback = R1 reversal. |
| R3 | **crs: fidelity expectations** (Risk 9): users read "imports LR presets/sidecars" as "renders identically". | `CrsImportReport` is first-class and surfaced (UI seam to E08/E16); mapping doc classifies every field exact/approximate/skipped; `lb:MigratedFrom` provenance recorded. Never silently drop — passthrough keeps originals. |
| R4 | **Sidecar-only writing** (mandate constraint 2) breaks interop with tools that only read embedded XMP in JPEG/DNG. | Accepted, stated limitation (documented in user docs + mapping doc). Read side covers embedded fully. A guarded opt-in embedded-write is a possible v1.x escalation to the mandate owner — explicitly out of E09. |
| R5 | **History table growth** at 100k images × heavy editing. | Delta rows are small + keyframes sparse; `ClearHistory` ships; optional history cap pref (default off) reserved — see OQ2. Storage math in §4. |
| R6 | **Schema-freeze pressure from E10/E11/E12**: a field the architecture didn't enumerate arrives mid-M2. | `lb_extra` + `unknown` maps absorb additive fields without a schema bump; real structural changes bump `RECIPE_SCHEMA` with a documented migration fn (`from_cbor` handles n→n+1 upgrades). Wire-id discipline (T1) makes additions cheap. |
| R7 | **Two-writer temptation**: E12's mask baking writing through recipe paths. | Ownership rule restated in code: `Recipe.masks` is ids-only; DAO exposes no API to embed content; snapshot self-containment goes through `SnapshotMaterializer` (T9) so E12 extends without touching `edit_recipe.doc` semantics. |
| R8 | **Gesture-commit granularity** wrong (too many/too few history steps) hurts UX and history size. | Gesture boundaries owned by the shell (pointer-down→up; text-field blur); `commit_gesture` API makes the policy explicit and testable; auto-commit-on-session-close guards dropped gestures. |

### Open questions (none block start; owners + due points named)

1. **OQ1 — LR sidecar write-back for `lr:hierarchicalSubject` escaping**: keywords containing `|` — LR's own behavior is lossy. Proposal: forbid `|` in keyword names at E07's input layer; escaping policy documented in T20. Decide with E07 owner by T20.
2. **OQ2 — History cap preference** (default unlimited, matching LR): ship the pref in E08's panel or defer entirely? Decide with E08 owner by M1 feature-freeze; schema needs nothing either way.
3. **OQ3 — `xmp_passthrough` storage form**: full original packet bytes (zstd, simplest, chosen as default) vs. pruned foreign-only subtree (smaller, riskier). T18 measures typical sizes; revisit only if p95 packet > 64 KB.
4. **OQ4 — Preset id collisions on shared files** (two users share a preset file, both edit): last-write-wins on `lb:PresetId`? Acceptable for v1; revisit with any future sync design. Note in T22 docs.
5. **OQ5 — Legacy pre-2012 (PV≤2) crs: tone params**: Adobe's exact 2010→2012 conversion is unpublished. We ship numeric best-effort + `approximate` classification (T19). If corpus review shows unacceptable results, downgrade PV≤2 develop import to skipped-with-report — decision at T19 review with E16 owner.
6. **OQ6 — `xmp:Rating`/label read at *import* time** (E04 seam): auto-read sidecar+embedded on import is E04's call; E09 provides the pure read path. Confirm the default (read-on-import: yes, LR-compatible) with E04 owner before M1 integration week.

---

## 8. Seams to neighboring epics (named, not designed)

| Epic | Seam artifact E09 provides | What the neighbor owns |
|---|---|---|
| **E01** | consumes: migration framework, writer handle, command bus, kill-9 harness, id newtypes (`ImageId`, `AssetId`) | foundation semantics; app-level undo stack mapping onto `StepTo` |
| **E05/E10** | `Recipe`, `ParamDelta`, `ProcessVersion`, `EditQuery::WorkingRecipe`, `EditCommitted` event | `RenderNode::invalidates(&ParamDelta)`, render scheduling, all pixel semantics; E10 UI panels emit gestures |
| **E12/E14** | `Recipe.masks`/`retouch` id lists, `MaskList`/`RetouchList` param ids, `SnapshotMaterializer` registry, reserved `edit_index` columns | mask/retouch tables + content, baked-raster policy (§4.5), mask XMP serialization |
| **E15** | `recipe_of`, `Recipe::to_xmp` + `library_map::to_xmp` for **exported-file** metadata packets (exports are new files — embedding allowed there) | encoders, metadata filtering tiers, export pipeline |
| **E16** | `from_lr_crs` + `CrsImportReport`, preset import path, mapping doc | `.lrcat` SQL extraction, migration UI/flow, bulk fidelity reporting |
| **E04** | `preset::apply_to_default` (apply-on-import), sidecar/embedded read path for import-time metadata | import pipeline, probe, when-to-read policy (OQ6) |
| **E07** | `WriteMetadata`/`ReadMetadata` commands, `library_map`, `XmpDivergence` query + `note_fs_event` | metadata tables/editor UI, fs-watcher, divergence badge rendering, keyword tree semantics |
| **E08** | `EditQuery::{History,Snapshots,Presets,PresetPreviewRecipe}`, typed errors for dialogs | history panel, preset browser + hover preview, copy/paste checklist, before/after UI, prefs panel (auto-write toggle, OQ2 cap) |
| **E06** | auto-write + sync jobs enqueued as `Class::Background` with `CancelToken` | scheduler, activity center surfacing |

---

## 9. Definition of done

E09 is done when **all** of the following hold:

1. All 27 tasks' acceptance criteria pass on the 3-OS CI matrix; PR-blocking suites (unit, property, fixtures, fast E2E, fuzz smoke, kill-9 fault injection) green.
2. **The M1 exit behaviors this epic underwrites work headlessly**: via `lightbox-cli` alone — edit an image's basic-panel params with history/snapshots persisted crash-safely; apply and create (partial) presets incl. an imported Lightroom preset; write sidecars; re-read them into a fresh catalog with struct-equal recipes and correct divergence states.
3. The §8 test-strategy rows owned by E09 (recipe/XMP property tests, crs: import fidelity, unknown-field preservation) exist in CI and are PR-blocking.
4. **License gates**: cargo-deny green with the new deps; XMP Toolkit registered in the surface-2 SBOM with the recorded **CTO sign-off**; no GPL anywhere in the E09 dependency set (exiftool used only as a dev-time test oracle subprocess, never shipped).
5. **Mandate guards hold**: no code path writes into an original file (T13 byte-identity test is permanent); catalog remains the single source of truth (divergence never auto-resolved); recipes are ordered parameter recipes exportable as XMP sidecars (constraint 2 satisfied end-to-end).
6. `docs/interop/crs-mapping.md` published and reviewed (E16 owner + one pixel-stream engineer), with every §3.2 field classified.
7. Perf numbers recorded in the nightly harness with the T7/T8/T26 targets met on the reference machine; no unresolved perf regressions filed against E09.
8. Public APIs rustdoc'd; the ownership rules (§3.1 single-owner, ids-only mask refs, projection-not-owner XMP) restated in crate-level docs; open questions OQ1–OQ6 each resolved or explicitly re-owned with a named epic.
9. Zero UI work shipped from this epic (the boundary held), and E10/E15/E16 planners have confirmed the seam artifacts in §8 are sufficient to start.
