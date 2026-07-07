// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Edit-store ↔ sidecar divergence (spec §3.5). Status is **computed, never
//! stored** — there is no fs-watcher in v2.0 (spec §0 item 6): it is derived at
//! open, after a write/read, and on explicit refresh by comparing the on-disk
//! sidecar hash and the recipe's `canonical_hash` against the last `xmp_sync`
//! stamps.
//!
//! # Scope
//!
//! This module ships the [`DivergenceStatus`] state machine and the pure
//! [`classify`] comparison. The `status(cat, asset, original)` entry point that
//! reads the `xmp_sync` row and stats the sidecar (spec §3.5) is wired in Phase B
//! once the `xmp_sync` DAO lands — see `E09-deviations.md` D-2.

/// Divergence status of an asset's sidecar versus the edit store (spec §3.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DivergenceStatus {
    /// No `.xmp` on disk.
    NoSidecar,
    /// A sidecar exists we never wrote or read (foreign — E16's honor-on-open
    /// entry point).
    SidecarOnly,
    /// Disk equals our last write and the recipe is unchanged since.
    InSync,
    /// The recipe changed since our last write; the disk sidecar is unchanged.
    CatalogNewer,
    /// The disk sidecar changed since our last write/read; the recipe is unchanged.
    SidecarNewer,
    /// Both changed.
    Conflict,
}

/// The last stamps recorded in `xmp_sync` for an asset (spec §4.1). Absent when
/// we have never written or read the sidecar.
#[derive(Clone, Copy, Debug, Default)]
pub struct SyncStamps {
    /// Sidecar content hash at our last write/read.
    pub sidecar_hash: Option<[u8; 16]>,
    /// Recipe `canonical_hash` at our last write/read.
    pub recipe_hash: Option<[u8; 16]>,
}

/// Pure divergence classification (spec §3.5 state machine). Inputs:
/// - `disk_hash`: hash of the sidecar now on disk, or `None` if no sidecar;
/// - `recipe_hash`: the recipe's current `canonical_hash`;
/// - `stamps`: what `xmp_sync` recorded at our last interaction.
pub fn classify(
    disk_hash: Option<[u8; 16]>,
    recipe_hash: [u8; 16],
    stamps: SyncStamps,
) -> DivergenceStatus {
    match (disk_hash, stamps.sidecar_hash) {
        // No sidecar on disk at all.
        (None, _) => DivergenceStatus::NoSidecar,
        // A sidecar exists but we have no record of touching it.
        (Some(_), None) => DivergenceStatus::SidecarOnly,
        // We have a prior interaction: compare both axes.
        (Some(disk), Some(known)) => {
            let sidecar_changed = disk != known;
            let recipe_changed = stamps.recipe_hash != Some(recipe_hash);
            match (sidecar_changed, recipe_changed) {
                (false, false) => DivergenceStatus::InSync,
                (false, true) => DivergenceStatus::CatalogNewer,
                (true, false) => DivergenceStatus::SidecarNewer,
                (true, true) => DivergenceStatus::Conflict,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: [u8; 16] = [1; 16];
    const B: [u8; 16] = [2; 16];
    const R1: [u8; 16] = [10; 16];
    const R2: [u8; 16] = [20; 16];

    #[test]
    fn no_sidecar() {
        assert_eq!(
            classify(None, R1, SyncStamps::default()),
            DivergenceStatus::NoSidecar
        );
    }

    #[test]
    fn foreign_sidecar_is_sidecar_only() {
        assert_eq!(
            classify(Some(A), R1, SyncStamps::default()),
            DivergenceStatus::SidecarOnly
        );
    }

    #[test]
    fn in_sync_after_own_write() {
        let stamps = SyncStamps {
            sidecar_hash: Some(A),
            recipe_hash: Some(R1),
        };
        assert_eq!(classify(Some(A), R1, stamps), DivergenceStatus::InSync);
    }

    #[test]
    fn edit_after_write_is_catalog_newer() {
        let stamps = SyncStamps {
            sidecar_hash: Some(A),
            recipe_hash: Some(R1),
        };
        assert_eq!(
            classify(Some(A), R2, stamps),
            DivergenceStatus::CatalogNewer
        );
    }

    #[test]
    fn external_edit_is_sidecar_newer() {
        let stamps = SyncStamps {
            sidecar_hash: Some(A),
            recipe_hash: Some(R1),
        };
        assert_eq!(
            classify(Some(B), R1, stamps),
            DivergenceStatus::SidecarNewer
        );
    }

    #[test]
    fn both_changed_is_conflict() {
        let stamps = SyncStamps {
            sidecar_hash: Some(A),
            recipe_hash: Some(R1),
        };
        assert_eq!(classify(Some(B), R2, stamps), DivergenceStatus::Conflict);
    }
}
