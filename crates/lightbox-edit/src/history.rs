// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! History vocabulary + reconstruction engine (spec §3.3, Phase B, `T9`).
//!
//! [`StepLabel`] is the label every durable edit txn carries into a
//! `history_step` row, a pure leaf type the preset ([`crate::preset`]) and
//! settings-transfer ([`crate::transfer`]) engines stamp onto the steps they
//! produce (shipped ahead of Phase B per E09-deviations E-2; **reused
//! verbatim here**, never redefined).
//!
//! [`list`]/[`recipe_at`]/[`clear`]/[`step_to`]/[`undo`]/[`redo`] are the T9
//! history engine: keyframe-every-64 reconstruction, LR-style truncate-on-
//! step-back-then-edit semantics (the truncation itself happens inside
//! [`crate::store::EditStore::commit`], §4.1-2 protocol step 1), and
//! undo/redo sugar over [`step_to`].

use serde::{Deserialize, Serialize};

use lightbox_catalog::Catalog;
use lightbox_types::{HistoryStepId, ImageId};

use crate::params::{ParamDelta, ParamId};
use crate::recipe::Recipe;
use crate::store::{decode_cbor, decode_recipe_doc, derive_index_fields, EditState, StoreError};

/// The label of a durable history step (spec §3.3). Serialized as CBOR into the
/// `history_step.op` column (Phase B) and surfaced to the history panel (E08).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum StepLabel {
    /// A single-param gesture, e.g. "Exposure" (the moved slider's [`ParamId`]).
    Param(ParamId),
    /// A preset was applied (carries the preset's display name).
    Preset {
        /// The applied preset's name.
        name: String,
    },
    /// Copied settings were pasted onto this image.
    Paste,
    /// Settings were synced from a source image onto this one.
    Sync,
    /// All edits were reset to the neutral default.
    Reset,
    /// A Lightroom `crs:` sidecar/preset was imported (E16 wiring; reserved now).
    CrsImport,
    /// A sidecar was read into the recipe as an (undoable) step.
    XmpRead,
    /// State was restored from a named snapshot.
    SnapshotRestore {
        /// The snapshot's name.
        name: String,
    },
    /// State was restored to an earlier history step.
    HistoryRestore {
        /// The target step sequence number.
        seq: u64,
    },
}

impl StepLabel {
    /// A short, stable, human-facing kind string (for logs / the E08 panel and
    /// for tests that assert which path produced a step).
    pub fn kind(&self) -> &'static str {
        match self {
            StepLabel::Param(_) => "param",
            StepLabel::Preset { .. } => "preset",
            StepLabel::Paste => "paste",
            StepLabel::Sync => "sync",
            StepLabel::Reset => "reset",
            StepLabel::CrsImport => "crs-import",
            StepLabel::XmpRead => "xmp-read",
            StepLabel::SnapshotRestore { .. } => "snapshot-restore",
            StepLabel::HistoryRestore { .. } => "history-restore",
        }
    }
}

// ── T9: history reconstruction & navigation ─────────────────────────────────

/// One history-step's metadata (spec §3.3), as returned newest-first by
/// [`list`] (the E08 history panel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryStepMeta {
    /// Row id.
    pub id: HistoryStepId,
    /// Dense per-image sequence number.
    pub seq: u64,
    /// The step's label.
    pub label: StepLabel,
    /// RFC3339 UTC.
    pub ts: String,
    /// `true` iff `seq == head_seq` (the current position).
    pub is_head: bool,
}

/// Newest-first step metadata for the E08 history panel (spec §3.3).
pub fn list(cat: &Catalog, image: ImageId) -> Result<Vec<HistoryStepMeta>, StoreError> {
    let reader = cat.reader();
    let head_seq = reader
        .edit_state_row(image)?
        .map(|r| r.head_seq)
        .unwrap_or(0);
    // Session-scale store (spec §7 R9): one page comfortably covers the
    // whole log; `ClearHistory` + the (reserved, OQ2) cap pref bound growth.
    let rows = reader.history_page(image, None, 10_000)?;
    rows.into_iter()
        .map(|row| {
            let label: StepLabel = decode_cbor(&row.op)?;
            Ok(HistoryStepMeta {
                id: row.id,
                seq: row.seq,
                label,
                ts: row.ts,
                is_head: row.seq == head_seq,
            })
        })
        .collect()
}

/// The recipe at history position `seq` (spec §3.3): nearest keyframe (every
/// 64th step) plus forward delta replay, `O(distance-to-anchor)`, not
/// `O(seq)`.
///
/// Two shortcuts, checked **in this order**: `seq == head_seq` returns the
/// durably-stored `doc` directly (no replay needed, `edit_recipe.doc`
/// *is* `recipe_at(head_seq)` by the §4.1 invariant); only then does
/// `seq == 0` return the neutral default. The order matters after
/// [`clear`]: it resets `head_seq` to 0 while **preserving** the current
/// (possibly non-neutral) doc, so `recipe_at(image, 0)` must still return
/// that doc, not neutral, once `head_seq` has collapsed to 0.
///
/// The upper bound for a valid `seq` is the highest seq **present in
/// `history_step`**, not `head_seq`: after `StepTo`/`Undo` moves `head_seq`
/// backward, later steps are kept (§4.1-2, "later steps kept until next
/// commit truncates them") precisely so `Redo` can replay forward past the
/// current `head_seq`, bounding on `head_seq` here would make `Redo`
/// perpetually fail with [`StoreError::NoSuchHistoryStep`].
pub fn recipe_at(cat: &Catalog, image: ImageId, seq: u64) -> Result<Recipe, StoreError> {
    let reader = cat.reader();
    let Some(row) = reader.edit_state_row(image)? else {
        return if seq == 0 {
            let pv = reader.image_detail(image)?.process_version;
            Ok(Recipe::identity(pv))
        } else {
            Err(StoreError::NoSuchHistoryStep { image, seq })
        };
    };
    let latest = reader.latest_history_seq(image)?.unwrap_or(0);
    if seq > row.head_seq.max(latest) {
        return Err(StoreError::NoSuchHistoryStep { image, seq });
    }
    if seq == row.head_seq {
        return decode_recipe_doc(image, &row.doc);
    }
    if seq == 0 {
        return Ok(Recipe::identity(row.pv));
    }
    let range = reader.history_replay_range(image, seq)?;
    let mut recipe = match range.anchor {
        Some((_, doc)) => decode_recipe_doc(image, &doc)?,
        None => Recipe::identity(row.pv),
    };
    for (_, delta_bytes) in range.deltas {
        let delta: ParamDelta = decode_cbor(&delta_bytes)?;
        recipe.apply(&delta)?;
    }
    Ok(recipe)
}

/// Drops the history log but **keeps the current doc** (spec §3.3 `clear`):
/// deletes every `history_step` row and resets `head_seq` to 0 in one txn.
/// A no-op on an untouched image (D1: nothing to clear).
///
/// See [`recipe_at`]'s doc comment for why this is safe: after a clear,
/// `head_seq == 0` and `seq == head_seq` is checked before `seq == 0`, so
/// `recipe_at(image, 0)` keeps returning the preserved doc, not neutral.
pub fn clear(cat: &Catalog, image: ImageId) -> Result<(), StoreError> {
    let reader = cat.reader();
    let Some(row) = reader.edit_state_row(image)? else {
        return Ok(());
    };
    drop(reader);
    cat.writer().with_txn(move |txn| {
        txn.truncate_history_after(image, 0)?;
        txn.upsert_edit_recipe(image, row.pv, row.schema, &row.doc, 0)?;
        Ok(())
    })?;
    Ok(())
}

/// Persists `doc`/`head_seq` to `seq` **without** deleting any steps beyond
/// it (spec §3.3/§4.1-2: "later steps kept until next commit truncates"
/// the truncation itself is [`crate::store::EditStore::commit`]'s job, so a
/// `StepTo` followed by a real edit drops exactly the right steps). Also
/// rebuilds `edit_index` (§4.1 invariant 1, same txn). `Undo`/`Redo` are
/// sugar on top of this.
pub fn step_to(cat: &Catalog, image: ImageId, seq: u64) -> Result<EditState, StoreError> {
    let recipe = recipe_at(cat, image, seq)?; // validates seq <= head_seq
    let doc = recipe.to_cbor();
    let (is_edited, has_masks, crop_ratio, treatment) = derive_index_fields(&recipe);
    let pv = recipe.pv;
    let schema = recipe.schema;
    cat.writer().with_txn(move |txn| {
        txn.upsert_edit_recipe(image, pv, schema, &doc, seq)?;
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
    let updated_at = cat.reader().edit_state_row(image)?.map(|r| r.updated_at);
    Ok(EditState {
        recipe,
        head_seq: seq,
        persisted: true,
        updated_at,
    })
}

/// `Undo` sugar: `StepTo(head_seq.saturating_sub(1))`, a no-op at the start
/// of history (matches Lightroom).
pub fn undo(cat: &Catalog, image: ImageId) -> Result<EditState, StoreError> {
    let head = current_head_seq(cat, image)?;
    step_to(cat, image, head.saturating_sub(1))
}

/// `Redo` sugar: `StepTo(head_seq + 1)` when a later step exists (kept by a
/// prior `StepTo`/`Undo` until the next real commit truncates it); a no-op
/// otherwise.
pub fn redo(cat: &Catalog, image: ImageId) -> Result<EditState, StoreError> {
    let head = current_head_seq(cat, image)?;
    let latest = cat.reader().latest_history_seq(image)?.unwrap_or(0);
    let target = if head < latest { head + 1 } else { head };
    step_to(cat, image, target)
}

fn current_head_seq(cat: &Catalog, image: ImageId) -> Result<u64, StoreError> {
    Ok(cat
        .reader()
        .edit_state_row(image)?
        .map(|r| r.head_seq)
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steplabel_cbor_round_trips() {
        for l in [
            StepLabel::Param(ParamId::Exposure),
            StepLabel::Preset {
                name: "Punchy".into(),
            },
            StepLabel::Paste,
            StepLabel::Sync,
            StepLabel::Reset,
            StepLabel::CrsImport,
            StepLabel::XmpRead,
            StepLabel::SnapshotRestore { name: "v2".into() },
            StepLabel::HistoryRestore { seq: 7 },
        ] {
            let mut buf = Vec::new();
            ciborium::ser::into_writer(&l, &mut buf).unwrap();
            let back: StepLabel = ciborium::de::from_reader(&buf[..]).unwrap();
            assert_eq!(back, l);
        }
    }

    // ── T9 acceptance criteria ──────────────────────────────────────────

    use crate::params::{ParamId as PId, ParamValue};
    use crate::store::{test_store_with_image, EditSession, EditStore};

    fn commit_exposure(store: &EditStore, image: ImageId, v: f32) {
        let state = store.open_state(image).unwrap();
        let mut session = EditSession::new(image, state);
        session.begin_gesture(StepLabel::Param(PId::Exposure));
        let mut d = ParamDelta::new();
        d.0.insert(PId::Exposure, ParamValue::F32(v));
        session.update(d).unwrap();
        let commit = session.take_commit().unwrap();
        store.commit(commit).unwrap();
    }

    #[test]
    fn recipe_at_matches_brute_force_replay() {
        let (_dir, store, image) = test_store_with_image();
        let values = [1.0, 1.5, -2.0, 0.25, 3.0, -1.0, 4.5, 0.0, 2.25, -3.5];
        for v in values {
            commit_exposure(&store, image, v);
        }
        for seq in 0..=values.len() as u64 {
            let got = recipe_at(store.catalog(), image, seq).unwrap();
            // Brute force: the neutral default with the seq-th value applied
            // (each commit here fully overwrites Exposure, so the expected
            // value at seq k is simply values[k-1], and neutral at seq 0).
            let expected = if seq == 0 {
                0.0
            } else {
                values[(seq - 1) as usize]
            };
            assert_eq!(
                got.get(PId::Exposure),
                ParamValue::F32(expected),
                "recipe_at({seq}) mismatch"
            );
        }
    }

    // §6 test plan: "History: recipe_at(k) ≡ brute-force replay ... for
    // random gesture sequences" (PR-blocking property test, T9 AC). Unlike
    // the fixed-value test above, this drives an independently-computed
    // brute-force fold (never calling `recipe_at`'s own anchor/replay path)
    // over a random multi-param gesture sequence long enough to cross the
    // `KEYFRAME_INTERVAL` (64) boundary at least sometimes, and checks every
    // `recipe_at(seq)` against it.
    mod proptests {
        use super::*;
        use crate::store::KEYFRAME_INTERVAL;
        use proptest::prelude::*;

        #[derive(Clone, Debug)]
        enum Gesture {
            Exposure(f32),
            Contrast(f32),
            Vibrance(f32),
        }

        fn arb_gesture() -> impl Strategy<Value = Gesture> {
            prop_oneof![
                (-5.0f32..=5.0).prop_map(Gesture::Exposure),
                (-100.0f32..=100.0).prop_map(Gesture::Contrast),
                (-100.0f32..=100.0).prop_map(Gesture::Vibrance),
            ]
        }

        fn commit_gesture(store: &EditStore, image: ImageId, g: &Gesture) -> Option<()> {
            let (id, value) = match *g {
                Gesture::Exposure(v) => (PId::Exposure, ParamValue::F32(v)),
                Gesture::Contrast(v) => (PId::Contrast, ParamValue::F32(v)),
                Gesture::Vibrance(v) => (PId::Vibrance, ParamValue::F32(v)),
            };
            let state = store.open_state(image).unwrap();
            let mut session = EditSession::new(image, state);
            session.begin_gesture(StepLabel::Param(id));
            let mut d = ParamDelta::new();
            d.0.insert(id, value);
            session.update(d).unwrap();
            // A gesture that lands back on the current value commits nothing
            // (§3.3 `take_commit`), a legitimate no-op the property must
            // tolerate, not a test bug (unlike the fixed-value tests above,
            // which hand-pick values to always net-change).
            let commit = session.take_commit()?;
            store.commit(commit).unwrap();
            Some(())
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(32))]

            #[test]
            fn recipe_at_matches_independent_brute_force_replay(
                gestures in prop::collection::vec(arb_gesture(), 1..(3 * KEYFRAME_INTERVAL as usize))
            ) {
                let (_dir, store, image) = test_store_with_image();
                let pv = store.open_state(image).unwrap().recipe.pv;

                // Brute-force fold: independent of `recipe_at`'s own anchor +
                // replay implementation, indexed by the seq actually reached
                // (skips gestures that didn't commit, exactly like the SUT).
                let mut expected = vec![Recipe::identity(pv)]; // expected[0] == seq 0
                for g in &gestures {
                    let before = store.open_state(image).unwrap().head_seq;
                    if commit_gesture(&store, image, g).is_some() {
                        let after = store.open_state(image).unwrap().head_seq;
                        prop_assert_eq!(after, before + 1);
                        let mut next = expected.last().unwrap().clone();
                        let (id, value) = match *g {
                            Gesture::Exposure(v) => (PId::Exposure, ParamValue::F32(v)),
                            Gesture::Contrast(v) => (PId::Contrast, ParamValue::F32(v)),
                            Gesture::Vibrance(v) => (PId::Vibrance, ParamValue::F32(v)),
                        };
                        let mut d = ParamDelta::new();
                        d.0.insert(id, value);
                        next.apply(&d).unwrap();
                        expected.push(next);
                    }
                }

                for (seq, want) in expected.iter().enumerate() {
                    let got = recipe_at(store.catalog(), image, seq as u64).unwrap();
                    prop_assert_eq!(&got, want, "recipe_at({}) diverged from brute-force replay", seq);
                }
            }
        }
    }

    #[test]
    fn recipe_at_head_matches_stored_doc_after_1000_steps_and_is_fast() {
        let (_dir, store, image) = test_store_with_image();
        // Exposure clamps to [-5.0, 5.0] (recipe.rs `set_scalar!`); walk a
        // 1000-step ramp strictly inside that range so every commit is a
        // real net change (the first step must differ from the neutral
        // 0.0 default) and no value clamps.
        let value = |i: u32| -4.0 + (i as f32) * 0.008;
        for i in 0..1000u32 {
            commit_exposure(&store, image, value(i));
        }
        let start = std::time::Instant::now();
        let at_head = recipe_at(store.catalog(), image, 1000).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(at_head.get(PId::Exposure), ParamValue::F32(value(999)));
        assert!(
            elapsed.as_millis() < 10,
            "recipe_at(head) took {elapsed:?}, want < 10ms"
        );

        let start = std::time::Instant::now();
        let mid = recipe_at(store.catalog(), image, 500).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(mid.get(PId::Exposure), ParamValue::F32(value(499)));
        assert!(
            elapsed.as_millis() < 10,
            "recipe_at(mid) took {elapsed:?}, want < 10ms"
        );
    }

    #[test]
    fn undo_redo_round_trips_head_seq() {
        let (_dir, store, image) = test_store_with_image();
        commit_exposure(&store, image, 1.0);
        commit_exposure(&store, image, 2.0);
        commit_exposure(&store, image, 3.0);
        assert_eq!(current_head_seq(store.catalog(), image).unwrap(), 3);

        let undone = undo(store.catalog(), image).unwrap();
        assert_eq!(undone.head_seq, 2);
        assert_eq!(undone.recipe.get(PId::Exposure), ParamValue::F32(2.0));

        let undone2 = undo(store.catalog(), image).unwrap();
        assert_eq!(undone2.head_seq, 1);

        let redone = redo(store.catalog(), image).unwrap();
        assert_eq!(redone.head_seq, 2);
        assert_eq!(
            redone.recipe, undone.recipe,
            "redo reproduces the same recipe"
        );

        let redone2 = redo(store.catalog(), image).unwrap();
        assert_eq!(redone2.head_seq, 3);
        assert_eq!(redone2.recipe.get(PId::Exposure), ParamValue::F32(3.0));

        // Redo past the latest existing step is a no-op.
        let redone3 = redo(store.catalog(), image).unwrap();
        assert_eq!(redone3.head_seq, 3);

        // Undo at the very start is a no-op.
        undo(store.catalog(), image).unwrap();
        undo(store.catalog(), image).unwrap();
        let at_zero = undo(store.catalog(), image).unwrap();
        assert_eq!(at_zero.head_seq, 0);
        assert!(at_zero.recipe.is_neutral());
    }

    #[test]
    fn step_back_then_edit_drops_later_steps_exactly() {
        let (_dir, store, image) = test_store_with_image();
        commit_exposure(&store, image, 1.0);
        commit_exposure(&store, image, 2.0);
        commit_exposure(&store, image, 3.0);
        commit_exposure(&store, image, 4.0);
        assert_eq!(current_head_seq(store.catalog(), image).unwrap(), 4);

        // Step back to seq=2 (keeps steps 3,4 in the table for now).
        step_to(store.catalog(), image, 2).unwrap();
        assert_eq!(
            store.catalog().reader().latest_history_seq(image).unwrap(),
            Some(4),
            "later steps are kept until the next commit"
        );

        // A brand-new edit from here must truncate steps 3,4 and land at 3.
        // (4.5 is within Exposure's [-5.0, 5.0] clamp range and distinct
        // from the seq=2 value of 2.0 it's edited from.)
        commit_exposure(&store, image, 4.5);
        let steps = store
            .catalog()
            .reader()
            .history_page(image, None, 10)
            .unwrap();
        assert_eq!(
            steps.len(),
            3,
            "seq 1,2,(new)3 -- steps 3,4 from before are gone"
        );
        assert_eq!(steps[0].seq, 3);
        let head = store.open_state(image).unwrap();
        assert_eq!(head.head_seq, 3);
        assert_eq!(head.recipe.get(PId::Exposure), ParamValue::F32(4.5));
    }

    #[test]
    fn clear_keeps_current_doc_and_drops_steps() {
        let (_dir, store, image) = test_store_with_image();
        commit_exposure(&store, image, 1.0);
        commit_exposure(&store, image, 2.0);
        clear(store.catalog(), image).unwrap();

        assert!(store
            .catalog()
            .reader()
            .history_page(image, None, 10)
            .unwrap()
            .is_empty());
        let row = store
            .catalog()
            .reader()
            .edit_state_row(image)
            .unwrap()
            .unwrap();
        assert_eq!(row.head_seq, 0);

        // The current doc (exposure=2.0) is PRESERVED, not reset to neutral.
        let at_zero = recipe_at(store.catalog(), image, 2).is_err();
        assert!(at_zero, "seq 2 no longer exists after clear");
        let current = recipe_at(store.catalog(), image, 0).unwrap();
        assert_eq!(
            current.get(PId::Exposure),
            ParamValue::F32(2.0),
            "clear keeps the current doc, not neutral, at the new seq 0"
        );
    }

    #[test]
    fn list_is_newest_first_with_is_head_flag() {
        let (_dir, store, image) = test_store_with_image();
        commit_exposure(&store, image, 1.0);
        commit_exposure(&store, image, 2.0);
        let steps = list(store.catalog(), image).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].seq, 2);
        assert!(steps[0].is_head);
        assert!(!steps[1].is_head);
        assert!(matches!(steps[0].label, StepLabel::Param(PId::Exposure)));
    }
}
