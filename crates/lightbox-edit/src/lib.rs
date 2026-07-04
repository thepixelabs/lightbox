// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-edit` — edit recipes, history, snapshots.
//!
//! **Status: stub + placeholder — owned by E09.** E01 freezes only the
//! [`Recipe`] placeholder below (spec §3.9): the `{ schema, pv }` fields and
//! `Recipe::identity()`. E09 replaces the body with the full architecture
//! §3.2 recipe type; `#[non_exhaustive]` keeps downstream crates honest until
//! then. `history_step`/`snapshot` tables are E09 migrations.

use lightbox_types::ProcessVersion;

/// M0 placeholder for the edit recipe (spec §3.9).
///
/// Only `schema`, `pv`, and [`Recipe::identity`] are frozen by E01 — everything
/// else about recipes belongs to E09.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Recipe {
    /// Recipe schema version (serialization format lineage, architecture §3.2).
    pub schema: u16,
    /// Pipeline process version this recipe renders under (architecture §4.5).
    pub pv: ProcessVersion,
}

impl Recipe {
    /// The identity recipe: renders the source unmodified under `pv`.
    pub fn identity(pv: ProcessVersion) -> Recipe {
        Recipe { schema: 1, pv }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_types::PV_M0;

    #[test]
    fn identity_recipe_shape() {
        let r = Recipe::identity(PV_M0);
        assert_eq!(r.schema, 1);
        assert_eq!(r.pv, PV_M0);
    }
}
