// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `camera_profile` registry: bundled-asset sync-on-open + the install /
//! query DAOs (E02 spec §4.1, task H1).
//!
//! The catalog stores color-asset **identity + provenance**, never the profile
//! bytes — the `.lblook` / `.dcp` files live in the app resources (bundled) or
//! the user profile dir (user-installed). Bundled entries are re-synced
//! idempotently every open (see [`Catalog::sync_bundled_profiles`]), keyed on
//! the natural `(kind, name, camera_make, camera_model)` identity with
//! NULL-aware `IS` matching (looks carry NULL make/model, which SQLite treats
//! as distinct under a UNIQUE constraint, exactly like `upsert_root`). A bundled
//! profile whose *content* changed (new [`BundledProfile::id`] / `file_hash`
//! under the same name) updates the existing row in place rather than
//! duplicating it — the model_pack re-sync contract E13 will follow.
//!
//! This module is intentionally free of any `lightbox-color` / `lightbox-decode`
//! dependency: the caller (the app / `lightbox-cli`) parses the `.lblook` /
//! `.dcp`, derives the stable `ProfileId` + file hash, and hands this crate the
//! finished [`BundledProfile`] records. The catalog owns only the row lifecycle.

use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::reader::ReaderHandle;
use crate::writer::CatalogTxn;

/// Which kind of color asset a registry row describes (mirrors the SQL
/// `CHECK (kind IN ('dcp','look'))`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileKind {
    /// A `.dcp` camera-matching profile (tier 3).
    Dcp,
    /// A `.lblook` Lightbox look (tier 2).
    Look,
}

impl ProfileKind {
    /// The lowercase token stored in `camera_profile.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileKind::Dcp => "dcp",
            ProfileKind::Look => "look",
        }
    }
}

/// Where a registered profile came from (mirrors `CHECK (source IN
/// ('bundled','user'))`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileSource {
    /// Shipped in the app resources; re-synced on open.
    Bundled,
    /// Installed by the user into their profile dir; never auto-removed.
    User,
}

impl ProfileSource {
    /// The lowercase token stored in `camera_profile.source`.
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileSource::Bundled => "bundled",
            ProfileSource::User => "user",
        }
    }
}

/// One color-asset registration heading into [`CatalogTxn::upsert_camera_profile`]
/// (E02 spec §4.1). The caller computes `id` (the `ProfileId` hex — xxh3-128 of
/// the canonical profile bytes) and `file_hash` before calling; the catalog
/// stores them verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundledProfile {
    /// `ProfileId` hex (xxh3-128 of the canonical serialization).
    pub id: String,
    /// `dcp` | `look`.
    pub kind: ProfileKind,
    /// Display name.
    pub name: String,
    /// Normalized camera make (`None` for looks).
    pub camera_make: Option<String>,
    /// Normalized camera model (`None` for looks).
    pub camera_model: Option<String>,
    /// Provenance source.
    pub source: ProfileSource,
    /// License token, mirroring the surface-3 manifest entry.
    pub license: String,
    /// Path to the on-disk profile file (app-resources- or profile-dir-relative
    /// per the caller's convention).
    pub file_path: String,
    /// Content hash of the profile file (the caller's canonical hex rendering).
    pub file_hash: String,
}

/// A registered color asset as read back from the catalog.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct InstalledProfile {
    /// `ProfileId` hex.
    pub id: String,
    /// `dcp` | `look`.
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Camera make (`None` for looks).
    pub camera_make: Option<String>,
    /// Camera model (`None` for looks).
    pub camera_model: Option<String>,
    /// `bundled` | `user`.
    pub source: String,
    /// License token.
    pub license: String,
    /// On-disk path.
    pub file_path: String,
    /// Content hash.
    pub file_hash: String,
    /// Unix-epoch seconds when the row was last (re)written.
    pub installed_at: i64,
}

/// What one profile upsert did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProfileUpsert {
    /// A brand-new row was inserted.
    Inserted,
    /// An existing row's content changed (new id/hash/path) and was rewritten.
    Updated,
    /// The stored row already matched — nothing written.
    Unchanged,
}

/// Aggregate outcome of a [`Catalog::sync_bundled_profiles`](crate::Catalog::sync_bundled_profiles)
/// pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SyncReport {
    /// Rows newly inserted.
    pub inserted: u32,
    /// Rows whose content changed and were rewritten.
    pub updated: u32,
    /// Rows that already matched.
    pub unchanged: u32,
}

impl SyncReport {
    /// Total rows considered.
    pub fn total(&self) -> u32 {
        self.inserted + self.updated + self.unchanged
    }
}

/// Current wall clock as unix-epoch seconds (registry `installed_at`).
fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl CatalogTxn<'_> {
    /// Idempotently registers one color asset (E02 spec §4.1). Matches an
    /// existing row on the natural `(kind, name, camera_make, camera_model)`
    /// identity with NULL-aware `IS` semantics; inserts when absent, rewrites in
    /// place when the content id / hash / path drifted, and no-ops when the
    /// stored row already matches.
    pub fn upsert_camera_profile(&mut self, p: &BundledProfile) -> Result<ProfileUpsert> {
        let existing: Option<(String, String, String, String, String)> = self
            .txn
            .query_row(
                "SELECT id, file_hash, file_path, source, license FROM camera_profile \
                 WHERE kind = ?1 AND name = ?2 AND camera_make IS ?3 AND camera_model IS ?4",
                params![p.kind.as_str(), p.name, p.camera_make, p.camera_model],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;

        match existing {
            Some((id, hash, path, source, license))
                if id == p.id
                    && hash == p.file_hash
                    && path == p.file_path
                    && source == p.source.as_str()
                    && license == p.license =>
            {
                Ok(ProfileUpsert::Unchanged)
            }
            Some(_) => {
                self.txn.execute(
                    "UPDATE camera_profile \
                     SET id = ?1, source = ?2, license = ?3, file_path = ?4, \
                         file_hash = ?5, installed_at = ?6 \
                     WHERE kind = ?7 AND name = ?8 AND camera_make IS ?9 AND camera_model IS ?10",
                    params![
                        p.id,
                        p.source.as_str(),
                        p.license,
                        p.file_path,
                        p.file_hash,
                        now_unix_secs(),
                        p.kind.as_str(),
                        p.name,
                        p.camera_make,
                        p.camera_model,
                    ],
                )?;
                Ok(ProfileUpsert::Updated)
            }
            None => {
                self.txn.execute(
                    "INSERT INTO camera_profile \
                     (id, kind, name, camera_make, camera_model, source, license, \
                      file_path, file_hash, installed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        p.id,
                        p.kind.as_str(),
                        p.name,
                        p.camera_make,
                        p.camera_model,
                        p.source.as_str(),
                        p.license,
                        p.file_path,
                        p.file_hash,
                        now_unix_secs(),
                    ],
                )?;
                Ok(ProfileUpsert::Inserted)
            }
        }
    }
}

impl ReaderHandle {
    /// All registered color assets, ordered by `(kind, name)` (stable output for
    /// the `lightbox-cli profile list` / test assertions).
    pub fn camera_profiles(&self) -> Result<Vec<InstalledProfile>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, kind, name, camera_make, camera_model, source, license, \
                    file_path, file_hash, installed_at \
             FROM camera_profile ORDER BY kind, name, camera_make, camera_model",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(InstalledProfile {
                id: r.get(0)?,
                kind: r.get(1)?,
                name: r.get(2)?,
                camera_make: r.get(3)?,
                camera_model: r.get(4)?,
                source: r.get(5)?,
                license: r.get(6)?,
                file_path: r.get(7)?,
                file_hash: r.get(8)?,
                installed_at: r.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One registered profile by its `ProfileId` hex, or `None`.
    pub fn camera_profile_by_id(&self, id: &str) -> Result<Option<InstalledProfile>> {
        self.conn()
            .query_row(
                "SELECT id, kind, name, camera_make, camera_model, source, license, \
                        file_path, file_hash, installed_at \
                 FROM camera_profile WHERE id = ?1",
                params![id],
                |r| {
                    Ok(InstalledProfile {
                        id: r.get(0)?,
                        kind: r.get(1)?,
                        name: r.get(2)?,
                        camera_make: r.get(3)?,
                        camera_model: r.get(4)?,
                        source: r.get(5)?,
                        license: r.get(6)?,
                        file_path: r.get(7)?,
                        file_hash: r.get(8)?,
                        installed_at: r.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(crate::error::CatalogError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    fn look(name: &str, id: &str, hash: &str) -> BundledProfile {
        BundledProfile {
            id: id.to_owned(),
            kind: ProfileKind::Look,
            name: name.to_owned(),
            camera_make: None,
            camera_model: None,
            source: ProfileSource::Bundled,
            license: "project".to_owned(),
            file_path: format!("color/looks/{name}.lblook"),
            file_hash: hash.to_owned(),
        }
    }

    fn dcp(name: &str, make: &str, model: &str, id: &str, hash: &str) -> BundledProfile {
        BundledProfile {
            id: id.to_owned(),
            kind: ProfileKind::Dcp,
            name: name.to_owned(),
            camera_make: Some(make.to_owned()),
            camera_model: Some(model.to_owned()),
            source: ProfileSource::Bundled,
            license: "project".to_owned(),
            file_path: format!("color/profiles/{make}/{model}.dcp"),
            file_hash: hash.to_owned(),
        }
    }

    #[test]
    fn sync_is_idempotent_and_updates_on_content_drift() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();

        let set = vec![
            look("lightbox-color-v1", "aaaa0001", "h1"),
            look("lightbox-neutral", "aaaa0002", "h2"),
            dcp("Sony ILCE-7M4", "Sony", "ILCE-7M4", "bbbb0001", "h3"),
        ];

        // First sync inserts everything.
        let r = catalog.sync_bundled_profiles(&set).unwrap();
        assert_eq!((r.inserted, r.updated, r.unchanged), (3, 0, 0));
        assert_eq!(catalog.reader().camera_profiles().unwrap().len(), 3);

        // A second identical sync is a pure no-op (bundled sync-on-open).
        let r = catalog.sync_bundled_profiles(&set).unwrap();
        assert_eq!((r.inserted, r.updated, r.unchanged), (0, 0, 3));
        assert_eq!(catalog.reader().camera_profiles().unwrap().len(), 3);

        // Content drift on one look (new id + hash, same name) rewrites in place
        // — no duplicate row despite the NULL make/model natural key.
        let mut drifted = set.clone();
        drifted[0] = look("lightbox-color-v1", "aaaa9999", "h1-v2");
        let r = catalog.sync_bundled_profiles(&drifted).unwrap();
        assert_eq!((r.inserted, r.updated, r.unchanged), (0, 1, 2));
        let rows = catalog.reader().camera_profiles().unwrap();
        assert_eq!(rows.len(), 3, "content drift must not duplicate a row");
        let updated = catalog
            .reader()
            .camera_profile_by_id("aaaa9999")
            .unwrap()
            .expect("rewritten row present under the new id");
        assert_eq!(updated.name, "lightbox-color-v1");
        assert_eq!(updated.file_hash, "h1-v2");
        assert!(
            catalog
                .reader()
                .camera_profile_by_id("aaaa0001")
                .unwrap()
                .is_none(),
            "stale id must be gone"
        );
    }
}
