// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! History vocabulary (spec §3.3).
//!
//! This module currently defines **only** [`StepLabel`], the label every durable
//! edit txn carries into a `history_step` row. It is a pure leaf type (it depends
//! only on [`ParamId`] and primitives) and is the seam artifact that the preset
//! ([`crate::preset`]) and settings-transfer ([`crate::transfer`]) engines stamp
//! onto the steps they produce.
//!
//! # Phase ordering note (E09-deviations E-2)
//!
//! The full history engine — `HistoryStepMeta`, `recipe_at`/`list`/`clear`,
//! keyframe replay, the `edit_recipe`/`history_step` DAO — is **Phase B**
//! (spec §3.3 / T5–T9), which is not present in this worktree. Phase E ships
//! [`StepLabel`] here (its spec home is `lightbox_edit::history`) so the preset
//! and transfer surfaces are self-consistent and testable now; Phase B extends
//! this module in place and **reuses this exact `StepLabel` definition** (Phase D
//! already references `StepLabel::XmpRead`, see deviations D-2).

use serde::{Deserialize, Serialize};

use crate::params::ParamId;

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
}
