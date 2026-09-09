// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The edit store (spec §3.1.1/§3.3, Phase B, `T6`/`T7`): [`EditStore`] wraps
//! `Arc<Catalog>` and is the sole `lightbox-edit` type that touches SQL (via
//! the public `lightbox-catalog` DAO/reader surface only, no raw SQL crosses
//! that crate's boundary, E01 rule, kept). [`EditSession`] is the pure,
//! in-memory gesture lifecycle the render/UI path reads during a drag; only
//! [`EditStore::commit`] (consuming a built [`PendingCommit`]) touches the DB.
//!
//! # D1 (persist-on-first-edit)
//!
//! [`EditStore::open_state`] and [`EditStore::recipe_of`] are **read-only**:
//! an untouched image gets no `edit_recipe` row (a synthesized neutral
//! [`Recipe`] instead), a 500-file folder drop performs zero edit-store
//! writes. The first committed gesture creates the row.
//!
//! # D2 (gesture updates are synchronous, in-memory, DB-free)
//!
//! [`EditSession::update`]/[`EditSession::working`] never touch the DB, the
//! T7 acceptance criterion is proven by `store::tests::working_recipe_reads_never_touch_the_db`,
//! which drops the backing [`Catalog`](lightbox_catalog::Catalog) entirely
//! before driving 500 updates.

use std::sync::{Arc, RwLock};

use serde::de::DeserializeOwned;
use serde::Serialize;

use lightbox_catalog::Catalog;
use lightbox_types::{ContentHash, ImageId, SnapshotId};

use crate::history::StepLabel;
use crate::leaves::{Crop, Treatment};
use crate::params::ParamDelta;
use crate::recipe::{Applied, Recipe, RecipeError, RecipeRead};
use crate::snapshot::{IdentityMaterializer, SnapshotMaterializer};

/// Every 64th committed step carries a full keyframe doc (spec §4.1: buys
/// O(distance) history stepping, T9's `recipe_at` nearest-anchor + replay).
pub(crate) const KEYFRAME_INTERVAL: u64 = 64;

/// Errors from the edit-store layer (spec §3.3, Phase B).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The catalog DAO/reader layer failed.
    #[error("catalog: {0}")]
    Catalog(#[from] lightbox_catalog::CatalogError),
    /// A recipe/delta failed to apply or validate.
    #[error("recipe: {0}")]
    Recipe(#[from] RecipeError),
    /// A doc was written by a newer Lightbox build; locked read-only.
    #[error("image {image:?}: recipe doc is schema {schema}, newer than this build supports")]
    NewerSchema {
        /// The image whose doc is affected.
        image: ImageId,
        /// The doc's declared schema.
        schema: u16,
    },
    /// `recipe_at`/`StepTo` was asked for a `seq` that does not exist.
    #[error("image {image:?}: history step seq {seq} does not exist")]
    NoSuchHistoryStep {
        /// The image queried.
        image: ImageId,
        /// The requested sequence number.
        seq: u64,
    },
    /// `CreateSnapshot`/promote hit the per-image `UNIQUE(image_id, name)`
    /// constraint, a typed error, never a silent overwrite (T10 AC).
    #[error("image {image:?}: a snapshot named {name:?} already exists")]
    DuplicateSnapshotName {
        /// The image the snapshot belongs to.
        image: ImageId,
        /// The name that collided.
        name: String,
    },
    /// `RestoreSnapshot`/delete/rename pointed at an unknown snapshot id.
    #[error("no such snapshot: {0:?}")]
    NoSuchSnapshot(SnapshotId),
    /// A CBOR blob (delta/inverse/op/snapshot doc) failed to decode.
    #[error("cbor decode failed: {0}")]
    Decode(String),
}

/// The durable-or-default edit state for one image (spec §3.3).
#[derive(Clone, Debug, PartialEq)]
pub struct EditState {
    /// The recipe (persisted, or the neutral default for an untouched image).
    pub recipe: Recipe,
    /// The history position `recipe` corresponds to.
    pub head_seq: u64,
    /// `true` iff an `edit_recipe` row actually exists (D1).
    pub persisted: bool,
    /// RFC3339 UTC, `None` for an unpersisted (untouched) image.
    pub updated_at: Option<String>,
}

/// The edit store's API (spec §3.1.1/§3.3). Wraps the E01 catalog handles;
/// cheap to clone via `Arc<EditStore>` at the call site (the type itself
/// holds only an `Arc<Catalog>` and a small registry lock).
pub struct EditStore {
    cat: Arc<Catalog>,
    materializer: RwLock<Arc<dyn SnapshotMaterializer>>,
}

impl EditStore {
    /// Wraps an already-open catalog.
    pub fn new(cat: Arc<Catalog>) -> EditStore {
        EditStore {
            cat,
            materializer: RwLock::new(Arc::new(IdentityMaterializer)),
        }
    }

    /// The backing catalog, a seam for the T8/T11/T12 follow-up (`EditHub`,
    /// the CLI, the kill-9 harness) to reach catalog concerns this store
    /// doesn't own (e.g. XMP sync stamps, §3.5).
    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.cat
    }

    /// Registers the active [`SnapshotMaterializer`] (the spec's "registry"
    /// a single overridable slot; M1 default is [`IdentityMaterializer`]; E12
    /// swaps in one that inlines mask/retouch content, §3.3).
    pub fn set_snapshot_materializer(&self, m: Arc<dyn SnapshotMaterializer>) {
        *self.materializer.write().unwrap_or_else(|e| e.into_inner()) = m;
    }

    pub(crate) fn materializer(&self) -> Arc<dyn SnapshotMaterializer> {
        self.materializer
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The open-time restore (spec §4.2). **Read-only** (D1): an untouched
    /// image creates no row; returns the persisted recipe, or the neutral
    /// default seeded from `image.process_version`.
    pub fn open_state(&self, image: ImageId) -> Result<EditState, StoreError> {
        let reader = self.cat.reader();
        match reader.edit_state_row(image)? {
            Some(row) => {
                let recipe = decode_recipe_doc(image, &row.doc)?;
                Ok(EditState {
                    recipe,
                    head_seq: row.head_seq,
                    persisted: true,
                    updated_at: Some(row.updated_at),
                })
            }
            None => {
                let pv = reader.image_detail(image)?.process_version;
                Ok(EditState {
                    recipe: Recipe::identity(pv),
                    head_seq: 0,
                    persisted: false,
                    updated_at: None,
                })
            }
        }
    }

    /// Read-only recipe fetch (WAL reader), E15 export, tooling. Unlike
    /// [`EditStore::open_state`] this surfaces a newer-schema doc as
    /// [`RecipeRead::NewerSchema`] rather than an error (callers that only
    /// need the raw bytes, e.g. a passthrough export, still get them).
    pub fn recipe_of(&self, image: ImageId) -> Result<RecipeRead, StoreError> {
        let reader = self.cat.reader();
        match reader.edit_state_row(image)? {
            Some(row) => Ok(Recipe::from_cbor(&row.doc)?),
            None => {
                let pv = reader.image_detail(image)?.process_version;
                Ok(RecipeRead::Ok(Recipe::identity(pv)))
            }
        }
    }

    /// The content-hash seam E04's loader calls after hashing a dropped
    /// file: the default image of the asset with this hash, if known.
    pub fn image_for_content_hash(&self, hash: ContentHash) -> Result<Option<ImageId>, StoreError> {
        Ok(self.cat.reader().image_for_content_hash(hash)?)
    }

    /// The DAO txn that consumes a [`PendingCommit`] (spec §4.1-2 commit
    /// protocol): truncate steps after `prev_head_seq` → append the new step
    /// → upsert `edit_recipe.doc`/`head_seq` → rebuild `edit_index`, all in
    /// **one** WAL txn via the E01 single writer.
    pub fn commit(&self, commit: PendingCommit) -> Result<EditState, StoreError> {
        let PendingCommit {
            image,
            new_recipe,
            prev_head_seq,
            label,
            delta,
            inverse,
        } = commit;
        let new_seq = prev_head_seq + 1;
        let op = encode_cbor(&label);
        let delta_bytes = encode_cbor(&delta);
        let inverse_bytes = encode_cbor(&inverse);
        let doc = new_recipe.to_cbor();
        let keyframe_doc = (new_seq % KEYFRAME_INTERVAL == 0).then(|| doc.clone());
        let (is_edited, has_masks, crop_ratio, treatment) = derive_index_fields(&new_recipe);
        let pv = new_recipe.pv;
        let schema = new_recipe.schema;

        self.cat.writer().with_txn(move |txn| {
            txn.truncate_history_after(image, prev_head_seq)?;
            txn.append_history_step(
                image,
                new_seq,
                &op,
                &delta_bytes,
                &inverse_bytes,
                keyframe_doc.as_deref(),
            )?;
            txn.upsert_edit_recipe(image, pv, schema, &doc, new_seq)?;
            txn.rebuild_edit_index(
                image,
                is_edited,
                has_masks,
                false,
                crop_ratio,
                Some(treatment),
            )?;
            Ok(())
        })?;

        let updated_at = self
            .cat
            .reader()
            .edit_state_row(image)?
            .map(|r| r.updated_at);
        Ok(EditState {
            recipe: new_recipe,
            head_seq: new_seq,
            persisted: true,
            updated_at,
        })
    }
}

// ── EditSession: the pure gesture lifecycle (T7) ────────────────────────────

struct GestureState {
    label: StepLabel,
}

/// In-memory working state for one open image (spec §3.3): one per open
/// image, owned by the core's `EditHub` (T8, follow-up). The render path
/// reads [`EditSession::working`], **never the DB**, during drags: every
/// method here is pure/in-memory; only [`EditStore::commit`] touches SQL.
pub struct EditSession {
    image: ImageId,
    committed: Recipe,
    working: Recipe,
    head_seq: u64,
    gesture: Option<GestureState>,
}

impl EditSession {
    /// Starts a session from a durable [`EditState`] (typically
    /// [`EditStore::open_state`]'s result).
    pub fn new(image: ImageId, state: EditState) -> EditSession {
        EditSession {
            image,
            committed: state.recipe.clone(),
            working: state.recipe,
            head_seq: state.head_seq,
            gesture: None,
        }
    }

    /// The image this session edits.
    pub fn image(&self) -> ImageId {
        self.image
    }

    /// The durable history position this session was last synced to.
    pub fn head_seq(&self) -> u64 {
        self.head_seq
    }

    /// The live in-memory recipe (render seam; never touches the DB).
    pub fn working(&self) -> &Recipe {
        &self.working
    }

    /// `true` between `begin_gesture` and `take_commit`/`cancel_gesture`.
    pub fn has_open_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Starts (or re-labels, if one is already open) a gesture: one gesture
    /// = one history step (spec §3.3). Re-labeling an open gesture does not
    /// discard in-progress `working` changes.
    pub fn begin_gesture(&mut self, label: StepLabel) {
        self.gesture = Some(GestureState { label });
    }

    /// Applies `delta` to the in-memory working recipe, coalesced: many
    /// `update` calls precede one `take_commit` (a 500-update slider drag is
    /// still exactly one history step). Never touches the DB.
    pub fn update(&mut self, delta: ParamDelta) -> Result<Applied, RecipeError> {
        self.working.apply(&delta)
    }

    /// Builds the commit payload (pure) and clears the open gesture.
    /// `None` when there is no open gesture, or the gesture produced no net
    /// change (e.g. a drag that returned to its start), nothing to durably
    /// commit, so a caller's `CommitGesture` becomes a no-op (T11 AC).
    pub fn take_commit(&mut self) -> Option<PendingCommit> {
        let gesture = self.gesture.take()?;
        if self.working == self.committed {
            return None;
        }
        let delta = self.working.diff(&self.committed);
        let inverse = self.committed.diff(&self.working);
        Some(PendingCommit {
            image: self.image,
            new_recipe: self.working.clone(),
            prev_head_seq: self.head_seq,
            label: gesture.label,
            delta,
            inverse,
        })
    }

    /// Discards the in-progress gesture, reverting `working` to `committed`.
    pub fn cancel_gesture(&mut self) {
        self.gesture = None;
        self.working = self.committed.clone();
    }

    /// Adopts a new durable state after a commit / `StepTo` / snapshot
    /// restore / sync landed for this image (whether or not this session
    /// itself produced it), e.g. after `EditStore::commit` returns, or a
    /// `history::step_to`/`restore_snapshot` result.
    pub fn apply_committed(&mut self, state: EditState) {
        self.committed = state.recipe.clone();
        self.working = state.recipe;
        self.head_seq = state.head_seq;
        self.gesture = None;
    }
}

/// The pure output of [`EditSession::take_commit`], everything
/// [`EditStore::commit`] needs to durably commit one gesture (spec §4.1-2).
/// Building this touches no I/O.
#[derive(Clone, Debug)]
pub struct PendingCommit {
    /// The target image.
    pub image: ImageId,
    /// The recipe the image should hold after this step.
    pub new_recipe: Recipe,
    /// The history position this commit is based on (becomes `new_seq - 1`).
    pub prev_head_seq: u64,
    /// The history step's label.
    pub label: StepLabel,
    /// Forward delta: `committed -> new_recipe`.
    pub delta: ParamDelta,
    /// Backward delta: `new_recipe -> committed`.
    pub inverse: ParamDelta,
}

// ── EditSink: wires the Phase-E settings-transfer engine to this store ─────
//
// Resolves E09-deviations.md E-4/E-5 (transfer.rs was written against the
// `EditSink` seam because `EditStore` did not exist yet). Not itself a T5-T10
// acceptance criterion, but it's a small, direct consequence of T6/T7/T9 now
// existing, and it closes a named deviation, so it ships here rather than
// waiting on the T8/T11/T12 follow-up.

impl crate::transfer::EditSink for EditStore {
    fn recipe_of(&self, image: ImageId) -> Result<Recipe, crate::transfer::TransferError> {
        use crate::transfer::TransferError;
        match EditStore::recipe_of(self, image).map_err(|e| TransferError::Store(e.to_string()))? {
            RecipeRead::Ok(r) => Ok(r),
            RecipeRead::NewerSchema { schema, .. } => Err(TransferError::Store(format!(
                "image {image:?}: recipe schema {schema} is newer than this build"
            ))),
        }
    }

    fn commit_batch(
        &self,
        batch: &[crate::transfer::CommitStep],
    ) -> Result<(), crate::transfer::TransferError> {
        use crate::transfer::TransferError;

        // Pre-read each target's current durable state (WAL-snapshot reads;
        // never block the writer). The single-EditSession-per-image
        // discipline the T8 EditHub registry enforces means nothing else
        // commits to these images between this read and the write txn below
        // in the product's actual usage pattern (sync/preset/paste targets
        // are batch-selected images, not the one image under interactive
        // edit), see E09-deviations.md for the narrow, accepted race window
        // this leaves open (a concurrent external mutation of the SAME
        // target mid-batch could be silently overwritten; unreachable via
        // any exposed command today).
        let mut prepared = Vec::with_capacity(batch.len());
        for step in batch {
            let base = match EditStore::recipe_of(self, step.image)
                .map_err(|e| TransferError::Store(e.to_string()))?
            {
                RecipeRead::Ok(r) => r,
                RecipeRead::NewerSchema { schema, .. } => {
                    return Err(TransferError::Store(format!(
                        "image {:?}: recipe schema {schema} is newer than this build",
                        step.image
                    )))
                }
            };
            let head_seq = self
                .cat
                .reader()
                .edit_state_row(step.image)
                .map_err(|e| TransferError::Store(e.to_string()))?
                .map(|r| r.head_seq)
                .unwrap_or(0);
            let delta = step.new_recipe.diff(&base);
            let inverse = base.diff(&step.new_recipe);
            prepared.push((step.clone(), head_seq, delta, inverse));
        }

        self.cat
            .writer()
            .with_txn(move |txn| {
                for (step, prev_head_seq, delta, inverse) in &prepared {
                    let new_seq = prev_head_seq + 1;
                    let op = encode_cbor(&step.label);
                    let delta_bytes = encode_cbor(delta);
                    let inverse_bytes = encode_cbor(inverse);
                    let doc = step.new_recipe.to_cbor();
                    let keyframe = (new_seq % KEYFRAME_INTERVAL == 0).then(|| doc.clone());
                    txn.truncate_history_after(step.image, *prev_head_seq)?;
                    txn.append_history_step(
                        step.image,
                        new_seq,
                        &op,
                        &delta_bytes,
                        &inverse_bytes,
                        keyframe.as_deref(),
                    )?;
                    txn.upsert_edit_recipe(
                        step.image,
                        step.new_recipe.pv,
                        step.new_recipe.schema,
                        &doc,
                        new_seq,
                    )?;
                    let (is_edited, has_masks, crop_ratio, treatment) =
                        derive_index_fields(&step.new_recipe);
                    txn.rebuild_edit_index(
                        step.image,
                        is_edited,
                        has_masks,
                        false,
                        crop_ratio,
                        Some(treatment),
                    )?;
                }
                Ok(())
            })
            .map_err(|e| TransferError::Store(e.to_string()))?;
        Ok(())
    }
}

// ── shared helpers (used by store/history/snapshot) ─────────────────────────

pub(crate) fn encode_cbor<T: Serialize>(v: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf).expect("in-memory CBOR encode is infallible");
    buf
}

pub(crate) fn decode_cbor<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    ciborium::de::from_reader(bytes).map_err(|e| StoreError::Decode(e.to_string()))
}

/// Decodes an `edit_recipe.doc`/keyframe blob, mapping [`RecipeRead::NewerSchema`]
/// to a typed [`StoreError`] (callers here always need a concrete [`Recipe`]).
pub(crate) fn decode_recipe_doc(image: ImageId, doc: &[u8]) -> Result<Recipe, StoreError> {
    match Recipe::from_cbor(doc)? {
        RecipeRead::Ok(r) => Ok(r),
        RecipeRead::NewerSchema { schema, .. } => Err(StoreError::NewerSchema { image, schema }),
    }
}

/// Derives `(is_edited, has_masks, crop_ratio, treatment)` from a decoded
/// recipe for `rebuild_edit_index` (spec §4.1: same txn as the doc write).
pub(crate) fn derive_index_fields(r: &Recipe) -> (bool, bool, Option<f64>, &'static str) {
    let is_edited = !r.is_neutral();
    let has_masks = !r.masks.is_empty();
    let crop_ratio = derive_crop_ratio(&r.geometry.crop);
    let treatment = match r.global.treatment {
        Treatment::Color => "color",
        Treatment::BlackAndWhite => "bw",
    };
    (is_edited, has_masks, crop_ratio, treatment)
}

/// Normalized crop aspect ratio; `None` for a full, un-cropped frame.
fn derive_crop_ratio(crop: &Crop) -> Option<f64> {
    let full = crop.left == 0.0 && crop.top == 0.0 && crop.right == 1.0 && crop.bottom == 1.0;
    if full {
        return None;
    }
    let w = f64::from(crop.right - crop.left);
    let h = f64::from(crop.bottom - crop.top);
    if h <= 0.0 {
        None
    } else {
        Some(w / h)
    }
}

// ── test support (shared by store/history/snapshot test modules) ───────────

#[cfg(test)]
pub(crate) fn test_store_with_image() -> (tempfile::TempDir, EditStore, ImageId) {
    let dir = tempfile::tempdir().expect("tempdir");
    let cat = Catalog::create(&dir.path().join("t.lbdata")).expect("create catalog");
    let image = cat
        .writer()
        .with_txn(|txn| {
            let root = txn.upsert_root(None, std::path::Path::new("/photos"))?;
            let folder = txn.upsert_folder(root, None, "shoot")?;
            let outcome = txn.insert_assets(&[lightbox_catalog::NewAsset {
                folder,
                filename: "a.jpg".to_owned(),
                content_hash: ContentHash([7u8; 16]),
                format: "JPEG".to_owned(),
                camera_make: None,
                camera_model: None,
                capture_time: None,
                width: 640,
                height: 480,
                orientation: lightbox_types::Orientation::O1,
                bytes: 1024,
                mtime_utc: None,
                decode_error: None,
                import_session: None,
            }])?;
            Ok(txn.insert_default_images(&outcome.inserted)?[0])
        })
        .expect("seed image");
    let store = EditStore::new(Arc::new(cat));
    (dir, store, image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{ParamId, ParamValue};
    use lightbox_types::PV_M0;

    fn exposure_delta(v: f32) -> ParamDelta {
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Exposure, ParamValue::F32(v));
        d
    }

    #[test]
    fn open_state_on_untouched_image_creates_no_row_d1() {
        let (_dir, store, image) = test_store_with_image();
        let state = store.open_state(image).unwrap();
        assert!(!state.persisted);
        assert_eq!(state.head_seq, 0);
        assert!(state.recipe.is_neutral());
        // D1 row-count probe: still no edit_recipe row after a read-only open.
        assert_eq!(
            store.catalog().reader().edit_state_row(image).unwrap(),
            None
        );
    }

    #[test]
    fn session_commit_writes_exactly_one_step_after_many_updates() {
        let (_dir, store, image) = test_store_with_image();
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);

        session.begin_gesture(StepLabel::Param(ParamId::Exposure));
        for i in 0..500 {
            session.update(exposure_delta(i as f32 * 0.001)).unwrap();
        }
        assert_eq!(
            session.working().get(ParamId::Exposure),
            ParamValue::F32(499.0 * 0.001)
        );
        let commit = session.take_commit().expect("net change -> a commit");
        assert_eq!(commit.prev_head_seq, 0);
        assert_eq!(commit.delta.len(), 1);

        let new_state = store.commit(commit).unwrap();
        assert_eq!(new_state.head_seq, 1);
        session.apply_committed(new_state);

        let history = store
            .catalog()
            .reader()
            .history_page(image, None, 10)
            .unwrap();
        assert_eq!(history.len(), 1, "500 updates -> exactly 1 committed step");
        assert_eq!(history[0].seq, 1);

        // Persisted, badges updated.
        let row = store
            .catalog()
            .reader()
            .edit_state_row(image)
            .unwrap()
            .unwrap();
        assert_eq!(row.head_seq, 1);
        assert!(store.catalog().reader().edit_badges(&[image]).unwrap()[0].is_edited);
    }

    #[test]
    fn no_net_change_gesture_commits_nothing() {
        let (_dir, store, image) = test_store_with_image();
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);
        session.begin_gesture(StepLabel::Param(ParamId::Exposure));
        session.update(exposure_delta(1.0)).unwrap();
        session.update(exposure_delta(0.0)).unwrap(); // back to neutral
        assert!(session.take_commit().is_none());
        assert_eq!(
            store.catalog().reader().edit_state_row(image).unwrap(),
            None
        );
    }

    #[test]
    fn cancel_gesture_reverts_working_and_commits_nothing() {
        let (_dir, store, image) = test_store_with_image();
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);
        session.begin_gesture(StepLabel::Param(ParamId::Exposure));
        session.update(exposure_delta(2.0)).unwrap();
        session.cancel_gesture();
        assert!(session.working().is_neutral());
        assert!(!session.has_open_gesture());
    }

    /// T7 AC: working-recipe reads never touch the DB. Proven by dropping the
    /// backing `Catalog` (and its writer thread) entirely, then driving 500
    /// updates + reads purely against the in-memory `EditSession`.
    #[test]
    fn working_recipe_reads_never_touch_the_db() {
        let (_dir, store, image) = test_store_with_image();
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);
        drop(store); // the catalog + writer thread are gone from here on

        session.begin_gesture(StepLabel::Param(ParamId::Exposure));
        for i in 0..500 {
            session.update(exposure_delta(i as f32 * 0.001)).unwrap();
        }
        assert_eq!(
            session.working().get(ParamId::Exposure),
            ParamValue::F32(499.0 * 0.001)
        );
        let commit = session.take_commit().unwrap();
        assert_eq!(commit.delta.len(), 1);
    }

    #[test]
    fn recipe_of_and_image_for_content_hash() {
        let (_dir, store, image) = test_store_with_image();
        assert_eq!(
            store.recipe_of(image).unwrap().into_recipe(),
            Some(Recipe::identity(PV_M0))
        );
        assert_eq!(
            store
                .image_for_content_hash(ContentHash([7u8; 16]))
                .unwrap(),
            Some(image)
        );
        assert_eq!(
            store
                .image_for_content_hash(ContentHash([9u8; 16]))
                .unwrap(),
            None
        );
    }

    #[test]
    fn edit_sink_sync_to_via_editstore() {
        use crate::transfer::{sync_to, NeverCancel, SyncOptions};
        let (_dir, store, image) = test_store_with_image();
        let report = sync_to(
            &store,
            &exposure_delta(1.25),
            &[image],
            StepLabel::Sync,
            &NeverCancel,
            SyncOptions::default(),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.committed, 1);
        let row = store
            .catalog()
            .reader()
            .edit_state_row(image)
            .unwrap()
            .unwrap();
        assert_eq!(row.head_seq, 1);
        let recipe = store.recipe_of(image).unwrap().into_recipe().unwrap();
        assert_eq!(recipe.get(ParamId::Exposure), ParamValue::F32(1.25));
    }
}
