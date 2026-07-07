// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Snapshots (spec §3.1/§3.3, Phase B — `T10`): named, self-contained
//! recipe projections, `RestoreSnapshot` (itself a history step),
//! promote-history-step-to-snapshot, and the [`SnapshotMaterializer`]
//! registry seam for E12.
//!
//! # Self-containment (§3.1 ownership rule)
//!
//! A snapshot doc must be an **immutable copy**, never a live owner of
//! mask/retouch content. The M1 default, [`IdentityMaterializer`], enforces
//! this the only way E09 can: it clears `masks`/`retouch` (E09 owns no mask
//! table to inline from). E12 registers a materializer that inlines that
//! content via [`EditStore::set_snapshot_materializer`].

use lightbox_types::{ImageId, SnapshotId};

use crate::history::StepLabel;
use crate::leaves::CborValue;
use crate::recipe::{Recipe, RecipeError, RecipeRead};
use crate::store::{EditState, EditStore, PendingCommit, StoreError};

/// A snapshot's browser metadata (spec §3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotMeta {
    /// Stable identity.
    pub id: SnapshotId,
    /// Display name (unique per image).
    pub name: String,
    /// RFC3339 UTC creation stamp.
    pub ts: String,
}

/// The E12 seam (spec §3.3): a snapshot doc must be **self-contained** — an
/// immutable copy, not a live owner of mask/retouch content (§3.1 ownership
/// rule). M1's default is [`IdentityMaterializer`]; E12 registers one that
/// inlines mask/retouch content into the snapshot doc.
pub trait SnapshotMaterializer: Send + Sync {
    /// Projects `r` into a self-contained snapshot doc.
    fn materialize(&self, r: &Recipe) -> Result<CborValue, RecipeError>;
    /// Reconstructs a `Recipe` from a snapshot doc (the inverse projection).
    fn restore(&self, doc: &CborValue, image: ImageId) -> Result<Recipe, RecipeError>;
}

/// The M1 default: **clears** (does not inline) mask/retouch id lists, since
/// their content lives in tables E09 does not own — the only self-contained
/// projection available without E12's materializer.
pub struct IdentityMaterializer;

impl SnapshotMaterializer for IdentityMaterializer {
    fn materialize(&self, r: &Recipe) -> Result<CborValue, RecipeError> {
        let mut stripped = r.clone();
        stripped.masks.clear();
        stripped.retouch.clear();
        let bytes = stripped.to_cbor();
        ciborium::de::from_reader(&bytes[..]).map_err(|e| RecipeError::Decode(e.to_string()))
    }

    fn restore(&self, doc: &CborValue, _image: ImageId) -> Result<Recipe, RecipeError> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(doc, &mut bytes)
            .map_err(|e| RecipeError::Decode(e.to_string()))?;
        match Recipe::from_cbor(&bytes)? {
            RecipeRead::Ok(r) => Ok(r),
            RecipeRead::NewerSchema { schema, .. } => Err(RecipeError::Decode(format!(
                "snapshot doc schema {schema} is newer than this build"
            ))),
        }
    }
}

impl EditStore {
    /// Materializes the **current** recipe and stores it as a named snapshot
    /// (spec §3.3/T10). Per-image name uniqueness is a typed
    /// [`StoreError::DuplicateSnapshotName`], never a silent overwrite.
    pub fn create_snapshot(&self, image: ImageId, name: &str) -> Result<SnapshotMeta, StoreError> {
        let state = self.open_state(image)?;
        self.create_snapshot_from_recipe(image, name, &state.recipe)
    }

    /// Promotes a past history step to a named snapshot (spec §3.3 T10:
    /// "promote-history-step-to-snapshot") — materializes `recipe_at(seq)`.
    pub fn promote_history_step(
        &self,
        image: ImageId,
        seq: u64,
        name: &str,
    ) -> Result<SnapshotMeta, StoreError> {
        let recipe = crate::history::recipe_at(self.catalog(), image, seq)?;
        self.create_snapshot_from_recipe(image, name, &recipe)
    }

    fn create_snapshot_from_recipe(
        &self,
        image: ImageId,
        name: &str,
        recipe: &Recipe,
    ) -> Result<SnapshotMeta, StoreError> {
        let materialized = self.materializer().materialize(recipe)?;
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&materialized, &mut bytes)
            .map_err(|e| StoreError::Decode(e.to_string()))?;
        let name_owned = name.to_owned();
        let id = self
            .catalog()
            .writer()
            .with_txn({
                let name = name_owned.clone();
                move |txn| txn.insert_snapshot(image, &name, &bytes)
            })
            .map_err(|e| match e {
                lightbox_catalog::CatalogError::Constraint(_) => {
                    StoreError::DuplicateSnapshotName {
                        image,
                        name: name_owned.clone(),
                    }
                }
                other => StoreError::Catalog(other),
            })?;
        // `insert_snapshot` returns only the id; re-fetch for the exact `ts`
        // the DAO stamped (avoids re-deriving the clock convention here).
        let ts = self
            .catalog()
            .reader()
            .snapshots(image)?
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| s.ts)
            .unwrap_or_default();
        Ok(SnapshotMeta {
            id,
            name: name_owned,
            ts,
        })
    }

    /// All snapshots for `image` (spec §3.4 `Queries::snapshots`).
    pub fn snapshots(&self, image: ImageId) -> Result<Vec<SnapshotMeta>, StoreError> {
        Ok(self
            .catalog()
            .reader()
            .snapshots(image)?
            .into_iter()
            .map(|r| SnapshotMeta {
                id: r.id,
                name: r.name,
                ts: r.ts,
            })
            .collect())
    }

    /// Deletes a snapshot by id.
    pub fn delete_snapshot(&self, snapshot: SnapshotId) -> Result<(), StoreError> {
        self.catalog()
            .writer()
            .with_txn(move |txn| txn.delete_snapshot(snapshot))?;
        Ok(())
    }

    /// Renames a snapshot (id is the stable identity).
    pub fn rename_snapshot(&self, snapshot: SnapshotId, name: &str) -> Result<(), StoreError> {
        let name = name.to_owned();
        self.catalog()
            .writer()
            .with_txn(move |txn| txn.rename_snapshot(snapshot, &name))?;
        Ok(())
    }

    /// Restores `snapshot` onto `image` **as a history step**
    /// (`StepLabel::SnapshotRestore`, spec §3.3/T10) — undoable, like every
    /// other durable mutation.
    pub fn restore_snapshot(
        &self,
        image: ImageId,
        snapshot: SnapshotId,
    ) -> Result<EditState, StoreError> {
        let row = self
            .catalog()
            .reader()
            .snapshots(image)?
            .into_iter()
            .find(|s| s.id == snapshot)
            .ok_or(StoreError::NoSuchSnapshot(snapshot))?;
        let value: CborValue = ciborium::de::from_reader(&row.recipe_doc[..])
            .map_err(|e| StoreError::Decode(e.to_string()))?;
        let restored = self.materializer().restore(&value, image)?;

        let state = self.open_state(image)?;
        let delta = restored.diff(&state.recipe);
        let inverse = state.recipe.diff(&restored);
        self.commit(PendingCommit {
            image,
            new_recipe: restored,
            prev_head_seq: state.head_seq,
            label: StepLabel::SnapshotRestore { name: row.name },
            delta,
            inverse,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{ParamDelta, ParamId, ParamValue};
    use crate::store::test_store_with_image;

    fn apply_exposure(store: &EditStore, image: ImageId, v: f32) {
        use crate::store::EditSession;
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);
        session.begin_gesture(StepLabel::Param(ParamId::Exposure));
        let mut d = ParamDelta::new();
        d.0.insert(ParamId::Exposure, ParamValue::F32(v));
        session.update(d).unwrap();
        let commit = session.take_commit().unwrap();
        store.commit(commit).unwrap();
    }

    #[test]
    fn create_mutate_restore_yields_struct_equal_recipe() {
        let (_dir, store, image) = test_store_with_image();
        apply_exposure(&store, image, 1.0);
        let at_v1 = store.open_state(image).unwrap().recipe;

        let meta = store.create_snapshot(image, "v1").unwrap();
        assert_eq!(meta.name, "v1");

        apply_exposure(&store, image, 2.0);
        assert_eq!(
            store
                .open_state(image)
                .unwrap()
                .recipe
                .get(ParamId::Exposure),
            ParamValue::F32(2.0)
        );

        let restored = store.restore_snapshot(image, meta.id).unwrap();
        assert_eq!(
            restored.recipe, at_v1,
            "restore reproduces the snapshotted recipe"
        );
        assert_eq!(
            store.open_state(image).unwrap().recipe,
            at_v1,
            "the durable state now matches the snapshot too"
        );

        // Restoring is itself a history step (undoable).
        let history = store
            .catalog()
            .reader()
            .history_page(image, None, 10)
            .unwrap();
        assert_eq!(history.len(), 3, "expose + expose + restore = 3 steps");
        assert_eq!(history[0].seq, 3);
    }

    #[test]
    fn snapshot_doc_unchanged_by_later_edits() {
        let (_dir, store, image) = test_store_with_image();
        apply_exposure(&store, image, 0.5);
        let meta = store.create_snapshot(image, "frozen").unwrap();
        let doc_before = store
            .catalog()
            .reader()
            .snapshots(image)
            .unwrap()
            .into_iter()
            .find(|s| s.id == meta.id)
            .unwrap()
            .recipe_doc;

        apply_exposure(&store, image, 9.9);

        let doc_after = store
            .catalog()
            .reader()
            .snapshots(image)
            .unwrap()
            .into_iter()
            .find(|s| s.id == meta.id)
            .unwrap()
            .recipe_doc;
        assert_eq!(doc_before, doc_after);
    }

    #[test]
    fn duplicate_snapshot_name_is_a_typed_error() {
        let (_dir, store, image) = test_store_with_image();
        apply_exposure(&store, image, 0.25);
        store.create_snapshot(image, "dup").unwrap();
        let err = store.create_snapshot(image, "dup").unwrap_err();
        assert!(
            matches!(err, StoreError::DuplicateSnapshotName { ref name, .. } if name == "dup"),
            "{err:?}"
        );
    }

    #[test]
    fn promote_history_step_to_snapshot() {
        let (_dir, store, image) = test_store_with_image();
        apply_exposure(&store, image, 1.0);
        apply_exposure(&store, image, 2.0);
        let at_seq1 = crate::history::recipe_at(store.catalog(), image, 1).unwrap();

        let meta = store.promote_history_step(image, 1, "step1").unwrap();
        let restored = store.restore_snapshot(image, meta.id).unwrap();
        assert_eq!(restored.recipe, at_seq1);
    }

    #[test]
    fn rename_and_delete_snapshot() {
        let (_dir, store, image) = test_store_with_image();
        apply_exposure(&store, image, 0.1);
        let meta = store.create_snapshot(image, "a").unwrap();
        store.rename_snapshot(meta.id, "b").unwrap();
        assert_eq!(store.snapshots(image).unwrap()[0].name, "b");
        store.delete_snapshot(meta.id).unwrap();
        assert!(store.snapshots(image).unwrap().is_empty());
        let err = store.restore_snapshot(image, meta.id).unwrap_err();
        assert!(matches!(err, StoreError::NoSuchSnapshot(_)));
    }
}
