// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `installed_look` DAO (E10 spec §5.2, task **D10**): write DAOs on
//! [`CatalogTxn`] and the matching read surface on [`ReaderHandle`] for the
//! `installed_look` table (migration `0007_installed_look`).
//!
//! Mirrors `profile_sync.rs`'s division of labor deliberately: this crate
//! stores identity + provenance only and never parses/validates/copies LUT
//! bytes, the caller (`lightbox-core`'s `looks` module) hashes the file,
//! parses+validates it (`lightbox-render`'s `.cube`/HaldCLUT parsers), copies
//! it into `<catalog>.lbdata/looks/`, and only THEN calls
//! [`CatalogTxn::install_look`] with the already-computed `content_hash` and
//! `rel_path`. No `lightbox-render`/filesystem dependency crosses into this
//! crate (spec §2.3 seam 1).
//!
//! Idempotent on `content_hash` (task D10 AC: "duplicate install of the same
//! bytes = idempotent, one row"), a second [`CatalogTxn::install_look`] call
//! with a hash already on file is a no-op that returns the existing row's id,
//! never a duplicate row (the table's own `UNIQUE(content_hash)` constraint
//! backs this, but the DAO checks first so it never even attempts, and
//! relies on catching, a constraint violation).

use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::reader::ReaderHandle;
use crate::writer::CatalogTxn;

/// Which LUT container a row's `rel_path` file is (mirrors the SQL `CHECK
/// (kind IN ('cube3d', 'haldclut'))`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LookKind {
    /// An Adobe/Iridas `.cube` 3D table.
    Cube3d,
    /// A HaldCLUT identity-image PNG.
    HaldClut,
}

impl LookKind {
    /// The lowercase token stored in `installed_look.kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            LookKind::Cube3d => "cube3d",
            LookKind::HaldClut => "haldclut",
        }
    }

    /// Parses the SQL token back; `None` on any other value (defensive, the
    /// CHECK constraint should make this unreachable on a healthy catalog,
    /// never a panic on read).
    pub fn from_str_token(s: &str) -> Option<LookKind> {
        match s {
            "cube3d" => Some(LookKind::Cube3d),
            "haldclut" => Some(LookKind::HaldClut),
            _ => None,
        }
    }
}

/// Provenance (mirrors the SQL `CHECK (source IN ('user', 'bundled'))`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LookSource {
    /// User-installed via the D11 look browser's "Install…" file picker.
    User,
    /// Shipped in app resources. Spec §4.8: v1 ships no bundled packs, this
    /// variant exists so a future bundled pack needs no schema change.
    Bundled,
}

impl LookSource {
    /// The lowercase token stored in `installed_look.source`.
    pub fn as_str(self) -> &'static str {
        match self {
            LookSource::User => "user",
            LookSource::Bundled => "bundled",
        }
    }
}

/// One row heading into [`CatalogTxn::install_look`]. The caller has already
/// hashed + validated + copied the file by the time this is built.
#[derive(Clone, Debug)]
pub struct NewInstalledLook {
    /// `.cube` vs HaldCLUT PNG.
    pub kind: LookKind,
    /// Display name (D11 browser).
    pub name: String,
    /// Browser grouping (D11 "grid w/ family grouping"); `None` = ungrouped.
    pub family: Option<String>,
    /// Path under `<catalog>.lbdata/looks/` (e.g. `"ab/ab12…ef.cube"`).
    pub rel_path: String,
    /// xxh3-128 hex of the file bytes, the `LookRef`/idempotency key.
    pub content_hash: String,
    /// Provenance.
    pub source: LookSource,
    /// REQUIRED when `source == Bundled` (surface-3 manifest id); `None` for
    /// every v1 (user-sourced) install.
    pub license: Option<String>,
}

/// One `installed_look` row, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledLookRow {
    /// Row id.
    pub id: i64,
    /// `"cube3d"` | `"haldclut"` (raw SQL token; [`LookKind::from_str_token`]
    /// parses it).
    pub kind: String,
    /// Display name.
    pub name: String,
    /// Browser grouping; `None` = ungrouped.
    pub family: Option<String>,
    /// Path under `<catalog>.lbdata/looks/`.
    pub rel_path: String,
    /// xxh3-128 hex of the file bytes.
    pub content_hash: String,
    /// `"user"` | `"bundled"` (raw SQL token).
    pub source: String,
    /// License token (bundled only).
    pub license: Option<String>,
    /// Unix seconds.
    pub installed_at: i64,
}

/// What [`CatalogTxn::install_look`] actually did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LookInstallOutcome {
    /// A brand-new row was inserted.
    Inserted,
    /// `content_hash` already existed, no row was written (idempotent
    /// install, task D10 AC).
    AlreadyInstalled,
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl CatalogTxn<'_> {
    /// Idempotently registers one installed look (task D10): a second call
    /// with the same `content_hash` is a no-op that returns the row id
    /// already on file, never a duplicate row.
    pub fn install_look(&mut self, row: &NewInstalledLook) -> Result<(i64, LookInstallOutcome)> {
        if let Some(id) = self
            .txn
            .query_row(
                "SELECT id FROM installed_look WHERE content_hash = ?1",
                params![row.content_hash],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok((id, LookInstallOutcome::AlreadyInstalled));
        }
        self.txn.execute(
            "INSERT INTO installed_look \
               (kind, name, family, rel_path, content_hash, source, license, installed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.kind.as_str(),
                row.name,
                row.family,
                row.rel_path,
                row.content_hash,
                row.source.as_str(),
                row.license,
                now_unix_secs(),
            ],
        )?;
        let id = self.txn.last_insert_rowid();
        Ok((id, LookInstallOutcome::Inserted))
    }

    /// Deletes one installed look by id (task D10: "removal deletes the
    /// row"). Returns the removed row (so the caller, `lightbox-core`'s
    /// `CatalogLookResolver`, can evict its parsed-LUT cache entry by
    /// `content_hash`), or `None` if the id was already gone. The on-disk
    /// file is intentionally left in place (no GC pass exists yet, module
    /// doc); recipes still referencing the hash keep their param and render
    /// identity (`CreativeLutNode`'s existing graceful-degrade contract)
    /// once the caller invalidates its resolver cache.
    pub fn remove_installed_look(&mut self, id: i64) -> Result<Option<InstalledLookRow>> {
        let existing = self
            .txn
            .query_row(
                "SELECT id, kind, name, family, rel_path, content_hash, source, license, \
                        installed_at \
                 FROM installed_look WHERE id = ?1",
                params![id],
                map_row,
            )
            .optional()?;
        if existing.is_some() {
            self.txn
                .execute("DELETE FROM installed_look WHERE id = ?1", params![id])?;
        }
        Ok(existing)
    }
}

impl ReaderHandle {
    /// Every installed look, grouped/sorted for the D11 browser (family,
    /// then name, SQLite sorts `NULL` family first ascending, i.e.
    /// "ungrouped" leads, matching `preset.rs`'s own ungrouped-first
    /// convention).
    pub fn all_installed_looks(&self) -> Result<Vec<InstalledLookRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, kind, name, family, rel_path, content_hash, source, license, \
                    installed_at \
             FROM installed_look ORDER BY family, name",
        )?;
        let rows = stmt.query_map([], map_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One installed look by content hash, the render engine's D10
    /// resolver's lookup primitive (`lightbox-core`'s `CatalogLookResolver`).
    pub fn installed_look_by_hash(&self, content_hash: &str) -> Result<Option<InstalledLookRow>> {
        self.conn()
            .query_row(
                "SELECT id, kind, name, family, rel_path, content_hash, source, license, \
                        installed_at \
                 FROM installed_look WHERE content_hash = ?1",
                params![content_hash],
                map_row,
            )
            .optional()
            .map_err(Into::into)
    }
}

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledLookRow> {
    Ok(InstalledLookRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        family: r.get(3)?,
        rel_path: r.get(4)?,
        content_hash: r.get(5)?,
        source: r.get(6)?,
        license: r.get(7)?,
        installed_at: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Catalog;

    fn row(name: &str, hash: &str, family: Option<&str>) -> NewInstalledLook {
        NewInstalledLook {
            kind: LookKind::Cube3d,
            name: name.to_owned(),
            family: family.map(str::to_owned),
            rel_path: format!("{}/{}.cube", &hash[0..2], hash),
            content_hash: hash.to_owned(),
            source: LookSource::User,
            license: None,
        }
    }

    /// **Task D10 AC:** installing the same content hash twice yields one
    /// row, not two, and reports [`LookInstallOutcome::AlreadyInstalled`] on
    /// the second call.
    #[test]
    fn duplicate_install_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let writer = catalog.writer();

        let r1 = row("Kodak Portra", "aaaa1111", Some("Film"));
        let (id1, outcome1) = writer.with_txn(move |txn| txn.install_look(&r1)).unwrap();
        assert_eq!(outcome1, LookInstallOutcome::Inserted);

        let r2 = row("Kodak Portra (dup path)", "aaaa1111", Some("Film"));
        let (id2, outcome2) = writer.with_txn(move |txn| txn.install_look(&r2)).unwrap();
        assert_eq!(outcome2, LookInstallOutcome::AlreadyInstalled);
        assert_eq!(id1, id2, "same content hash resolves to the SAME row");

        let all = catalog.reader().all_installed_looks().unwrap();
        assert_eq!(all.len(), 1, "duplicate install left exactly one row");
    }

    /// **Task D10 AC:** removal deletes the row; a repeat removal of the same
    /// id is a harmless no-op (`None`), never an error.
    #[test]
    fn remove_deletes_the_row_and_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let writer = catalog.writer();

        let r = row("Fuji Velvia", "bbbb2222", None);
        let (id, _) = writer.with_txn(move |txn| txn.install_look(&r)).unwrap();
        assert_eq!(catalog.reader().all_installed_looks().unwrap().len(), 1);

        let removed = writer
            .with_txn(move |txn| txn.remove_installed_look(id))
            .unwrap();
        assert_eq!(removed.map(|r| r.content_hash), Some("bbbb2222".to_owned()));
        assert!(catalog.reader().all_installed_looks().unwrap().is_empty());

        // Removing again is a harmless no-op.
        let removed_again = writer
            .with_txn(move |txn| txn.remove_installed_look(id))
            .unwrap();
        assert_eq!(removed_again, None);
    }

    /// A removed look's `content_hash` no longer resolves, the resolver
    /// cache-invalidation seam's read-side precondition (`lightbox-core`'s
    /// `CatalogLookResolver` calls `installed_look_by_hash` on cache miss).
    #[test]
    fn installed_look_by_hash_is_none_after_removal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let writer = catalog.writer();

        let r = row("Agfa Vista", "cccc3333", Some("Film"));
        let (id, _) = writer.with_txn(move |txn| txn.install_look(&r)).unwrap();
        assert!(catalog
            .reader()
            .installed_look_by_hash("cccc3333")
            .unwrap()
            .is_some());

        writer
            .with_txn(move |txn| txn.remove_installed_look(id))
            .unwrap();
        assert!(catalog
            .reader()
            .installed_look_by_hash("cccc3333")
            .unwrap()
            .is_none());
    }

    /// Listing is grouped/sorted `(family, name)`, ungrouped (`NULL` family)
    /// first, the D11 browser's expected iteration order.
    #[test]
    fn listing_is_sorted_by_family_then_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let writer = catalog.writer();

        for (name, hash, family) in [
            ("Zeta", "h1", Some("Film")),
            ("Alpha", "h2", None),
            ("Alpha", "h3", Some("Film")),
            ("Beta", "h4", Some("B&W")),
        ] {
            let r = row(name, hash, family);
            writer.with_txn(move |txn| txn.install_look(&r)).unwrap();
        }

        let all = catalog.reader().all_installed_looks().unwrap();
        let order: Vec<(Option<String>, String)> = all
            .iter()
            .map(|r| (r.family.clone(), r.name.clone()))
            .collect();
        assert_eq!(
            order,
            vec![
                (None, "Alpha".to_owned()),
                (Some("B&W".to_owned()), "Beta".to_owned()),
                (Some("Film".to_owned()), "Alpha".to_owned()),
                (Some("Film".to_owned()), "Zeta".to_owned()),
            ]
        );
    }

    /// [`LookKind`]/[`LookSource`] token round-trip (defensive read-path
    /// coverage, the CHECK constraint should make an unparseable token
    /// unreachable on a healthy catalog, but the parser must never panic).
    #[test]
    fn look_kind_token_round_trips() {
        assert_eq!(
            LookKind::from_str_token(LookKind::Cube3d.as_str()),
            Some(LookKind::Cube3d)
        );
        assert_eq!(
            LookKind::from_str_token(LookKind::HaldClut.as_str()),
            Some(LookKind::HaldClut)
        );
        assert_eq!(LookKind::from_str_token("bogus"), None);
    }
}
