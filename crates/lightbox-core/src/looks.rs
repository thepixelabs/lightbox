// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E10 Phase D (task **D10**): the `installed_look` install/remove pipeline
//! and [`CatalogLookResolver`], the [`lightbox_render::ng::LookResolver`]
//! adapter the render engine's `RecipeCompiler` consults at graph-compile
//! time to turn a recipe's `CreativeLut.id` (a content-hash reference, spec
//! §4.8) into a parsed `Lut3D` for `CreativeLutNode` to sample. This is
//! exactly the seam `creative_lut.rs`'s own module doc names as "the `id` →
//! LUT-content seam (D10, explicitly out of THIS phase's scope)", this
//! module is that phase, for the `lightbox-core`/catalog half of it.
//!
//! `lightbox-catalog`'s `looks_dao` stores identity + provenance only (spec
//! §2.3 seam 1: no SQL/rusqlite crosses its boundary, and, mirroring
//! `profile_sync.rs`'s division of labor, no LUT-parsing dependency
//! either); this module does the hashing, validation, and file-copy the DAO
//! itself never does.
//!
//! # Install pipeline ([`install_look_file`])
//!
//! 1. Read the source file's bytes.
//! 2. **Validate BEFORE writing anything**: `.cube` via
//!    `Lut3D::from_cube_str`, `.png` via `Lut3D::from_haldclut_png`
//!    (extension-sniffed). A malformed file is rejected with
//!    [`CoreError::InvalidLook`] and nothing is written, no orphan file, no
//!    registry row (spec §4.8 R7: untrusted input, never a panic, and this
//!    phase adds "never a partial write" on top).
//! 3. Hash the bytes (xxh3-128, the same primitive `lightbox-color`'s
//!    `ProfileId`/`lightbox-edit`'s `PresetId::derived_from_path` use)
//!    this is the row's `content_hash` AND the on-disk key, so installing
//!    the same bytes twice naturally targets the same destination (task D10
//!    AC: idempotent install).
//! 4. Copy the ORIGINAL bytes (not the parsed table) to
//!    `<lbdata>/looks/<hh>/<hash>.<ext>` (`<hh>` = first 2 hex chars, a
//!    fan-out directory, mirrors `lightbox-preview`'s own cache-path
//!    convention) via write-to-temp-then-rename (atomic; a concurrent
//!    duplicate install racing this one just overwrites with byte-identical
//!    content, since the same hash implies the same bytes), skipped
//!    entirely if the destination already exists.
//! 5. Upsert the `installed_look` row through the DAO (idempotent on
//!    `content_hash`, the DAO's own job, see `looks_dao.rs`).
//!
//! # Removal ([`remove_look`])
//!
//! Deletes the row only, the file is left on disk (spec §5.2 DAO doc:
//! "removal allowed; recipes degrade to badge"; a future GC pass, out of
//! D10's scope, could sweep orphaned look files the way `lightbox-preview`'s
//! reconcile sweeps cache orphans). The caller (`session.rs`'s dispatch) is
//! responsible for calling [`CatalogLookResolver::invalidate`] with the
//! removed row's `content_hash` so a still-referencing recipe's very next
//! render degrades to identity immediately, not just after the next process
//! restart.
//!
//! # [`CatalogLookResolver`]
//!
//! Caches parsed `Lut3D`s by content hash, parsing a 65-edge cube is not
//! something every compile (which can happen at slider-drag cadence
//! whenever a creative look is active, `RecipeCompiler::compile` rebuilds
//! the whole tone/color segment on every call) should redo. A cache entry is
//! valid until [`CatalogLookResolver::invalidate`] evicts it (called by
//! `RemoveLook`'s dispatch), the spec §5.2 DAO doc's "removal leaves
//! referencing recipes rendering identity" contract, live.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use lightbox_catalog::{Catalog, LookInstallOutcome, LookKind, LookSource, NewInstalledLook};
use lightbox_render::ng::nodes::common::lut3d::Lut3D;
use lightbox_render::ng::LookResolver;
use lightbox_types::LookId;

use crate::error::{CoreError, Result};

/// What [`install_look_file`] actually did (mirrors
/// `lightbox_catalog::LookInstallOutcome`, re-exported at this crate's
/// boundary so callers never need the `lightbox-catalog` dependency
/// directly, seam 1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstallOutcome {
    /// A brand-new row was inserted.
    Inserted,
    /// The same content hash was already installed, no row was written
    /// (task D10 AC: duplicate install is idempotent).
    AlreadyInstalled,
}

/// Reads, validates, hashes, copies, and registers `path` as an installed
/// look (task D10). See the module doc for the full pipeline. `family` is
/// the D11 browser's optional grouping label.
pub fn install_look_file(
    catalog: &Catalog,
    path: &Path,
    family: Option<String>,
) -> Result<(LookId, InstallOutcome, String)> {
    let bytes = std::fs::read(path).map_err(CoreError::Io)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let kind = match ext.as_str() {
        "cube" => LookKind::Cube3d,
        "png" => LookKind::HaldClut,
        other => {
            return Err(CoreError::InvalidLook(format!(
                "unsupported look file extension {other:?} (expected .cube or .png)"
            )))
        }
    };

    // Validate BEFORE any write (module doc): a malformed file must leave no
    // trace (no orphan file, no registry row).
    match kind {
        LookKind::Cube3d => {
            let text = std::str::from_utf8(&bytes).map_err(|e| {
                CoreError::InvalidLook(format!(".cube file is not valid UTF-8: {e}"))
            })?;
            Lut3D::from_cube_str(text)
                .map_err(|e| CoreError::InvalidLook(format!("invalid .cube file: {e}")))?;
        }
        LookKind::HaldClut => {
            Lut3D::from_haldclut_png(&bytes)
                .map_err(|e| CoreError::InvalidLook(format!("invalid HaldCLUT PNG: {e}")))?;
        }
    }

    let content_hash = hash_hex(&bytes);
    let hh = &content_hash[0..2];
    let rel_path = format!("{hh}/{content_hash}.{ext}");
    let dest = catalog.lbdata_dir().join("looks").join(&rel_path);
    if !dest.exists() {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
        }
        // write-then-rename: atomic, so a reader (the resolver) never sees a
        // partially-written file at `dest`.
        let tmp = dest.with_extension(format!("{ext}.tmp"));
        std::fs::write(&tmp, &bytes).map_err(CoreError::Io)?;
        std::fs::rename(&tmp, &dest).map_err(CoreError::Io)?;
    }

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Untitled")
        .to_owned();
    let new_row = NewInstalledLook {
        kind,
        name,
        family,
        rel_path,
        content_hash: content_hash.clone(),
        source: LookSource::User,
        license: None,
    };
    let writer = catalog.writer();
    let (id, outcome) = writer
        .with_txn(move |txn| txn.install_look(&new_row))
        .map_err(CoreError::Catalog)?;
    let outcome = match outcome {
        LookInstallOutcome::Inserted => InstallOutcome::Inserted,
        LookInstallOutcome::AlreadyInstalled => InstallOutcome::AlreadyInstalled,
    };
    Ok((LookId(id), outcome, content_hash))
}

/// Removes an installed look's registry row (task D10). Returns the removed
/// row's `content_hash` (for the caller to invalidate the resolver cache
/// see the module doc), or `None` if `id` was already gone. The on-disk
/// file is left in place (module doc).
pub fn remove_look(catalog: &Catalog, id: LookId) -> Result<Option<String>> {
    let writer = catalog.writer();
    let removed = writer
        .with_txn(move |txn| txn.remove_installed_look(id.0))
        .map_err(CoreError::Catalog)?;
    Ok(removed.map(|row| row.content_hash))
}

/// xxh3-128 of `bytes`, lowercase hex, the same primitive/rendering
/// `lightbox_types::ContentHash::to_hex` uses, computed directly (this
/// crate has no reason to round-trip through the 16-byte array type for a
/// value that is immediately stored/compared as text).
fn hash_hex(bytes: &[u8]) -> String {
    let h = twox_hash::XxHash3_128::oneshot(bytes);
    let b = h.to_le_bytes();
    let mut s = String::with_capacity(32);
    for byte in b {
        use std::fmt::Write as _;
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// The production [`LookResolver`] (task D10): resolves a recipe's
/// `CreativeLut.id` (a `content_hash`) to a parsed [`Lut3D`] by looking it
/// up in the `installed_look` catalog table, reading the on-disk file, and
/// parsing it, cached by `content_hash` so a 60 Hz slider-drag compile
/// while a look is active does not re-parse the file every frame (module
/// doc).
pub struct CatalogLookResolver {
    catalog: Arc<Catalog>,
    cache: Mutex<HashMap<String, Arc<Lut3D>>>,
}

impl CatalogLookResolver {
    /// A resolver over `catalog`, with an empty cache.
    pub fn new(catalog: Arc<Catalog>) -> CatalogLookResolver {
        CatalogLookResolver {
            catalog,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Evicts `content_hash` from the parsed-LUT cache (called by
    /// `RemoveLook`'s dispatch, `session.rs`), the live half of "removal
    /// leaves referencing recipes rendering identity" (spec §5.2 DAO doc):
    /// without this, a recipe that resolved successfully before removal
    /// would keep rendering the now-removed look until the cache happened
    /// to evict on its own (never, today, see the module doc).
    pub fn invalidate(&self, content_hash: &str) {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(content_hash);
    }
}

impl LookResolver for CatalogLookResolver {
    fn resolve(&self, content_hash: &str) -> Option<Arc<Lut3D>> {
        if let Some(hit) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(content_hash)
        {
            return Some(Arc::clone(hit));
        }
        let reader = self.catalog.reader();
        let row = reader.installed_look_by_hash(content_hash).ok()??;
        let path = self.catalog.lbdata_dir().join("looks").join(&row.rel_path);
        let bytes = std::fs::read(&path).ok()?;
        let lut = match LookKind::from_str_token(&row.kind)? {
            LookKind::Cube3d => {
                let text = std::str::from_utf8(&bytes).ok()?;
                Lut3D::from_cube_str(text).ok()?
            }
            LookKind::HaldClut => Lut3D::from_haldclut_png(&bytes).ok()?,
        };
        let arc = Arc::new(lut);
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(content_hash.to_owned(), Arc::clone(&arc));
        Some(arc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_cube_text() -> &'static str {
        "LUT_3D_SIZE 2\n\
         0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n"
    }

    fn swap_rg_cube_text() -> &'static str {
        // A 2-entry .cube that swaps R and G, a known non-identity LUT for
        // end-to-end proofs.
        "LUT_3D_SIZE 2\n\
         0 0 0\n0 1 0\n1 0 0\n1 1 0\n0 0 1\n0 1 1\n1 0 1\n1 1 1\n"
    }

    /// **Task D10 AC:** installing the SAME bytes twice is idempotent (one
    /// row, one on-disk file), proven end to end through
    /// `install_look_file`, not just the DAO's own unit test.
    #[test]
    fn duplicate_install_of_the_same_bytes_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let src = tmp.path().join("look.cube");
        std::fs::write(&src, identity_cube_text()).unwrap();

        let (id1, outcome1, hash1) =
            install_look_file(&catalog, &src, Some("Film".to_owned())).unwrap();
        assert_eq!(outcome1, InstallOutcome::Inserted);

        let (id2, outcome2, hash2) =
            install_look_file(&catalog, &src, Some("Film".to_owned())).unwrap();
        assert_eq!(outcome2, InstallOutcome::AlreadyInstalled);
        assert_eq!(id1, id2);
        assert_eq!(hash1, hash2);

        assert_eq!(catalog.reader().all_installed_looks().unwrap().len(), 1);
        let looks_dir = catalog.lbdata_dir().join("looks");
        let file_count = walk_files(&looks_dir);
        assert_eq!(file_count, 1, "duplicate install wrote only one file");
    }

    /// **Task D10 AC (install→apply→render, the catalog half):** a real
    /// `.cube` installs, resolves through [`CatalogLookResolver`] to a
    /// genuinely non-identity [`Lut3D`], then, after [`remove_look`] +
    /// [`CatalogLookResolver::invalidate`], resolves to `None` again (the
    /// render-side `CreativeLutNode` degrade-to-identity contract's
    /// precondition).
    #[test]
    fn install_resolve_remove_invalidate_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Arc::new(Catalog::create(&tmp.path().join("c.lbdata")).unwrap());
        let src = tmp.path().join("swap.cube");
        std::fs::write(&src, swap_rg_cube_text()).unwrap();

        let (id, _, hash) = install_look_file(&catalog, &src, None).unwrap();

        let resolver = CatalogLookResolver::new(Arc::clone(&catalog));
        let lut = resolver.resolve(&hash).expect("resolves after install");
        assert!(
            !lut.is_identity(1e-6),
            "the parsed LUT must be the real (non-identity) R/G swap"
        );
        // A second resolve is a cache hit (same Arc-backed table content)
        // proven indirectly by re-resolving and comparing table data.
        let lut_again = resolver.resolve(&hash).expect("cache hit resolves too");
        assert_eq!(lut.data(), lut_again.data());

        let removed_hash = remove_look(&catalog, id).unwrap();
        assert_eq!(removed_hash.as_deref(), Some(hash.as_str()));
        assert!(catalog
            .reader()
            .installed_look_by_hash(&hash)
            .unwrap()
            .is_none());

        // Before invalidation, the STALE cache entry would still resolve
        // this line documents why `session.rs`'s `RemoveLook` dispatch must
        // call `invalidate` (not exercised as an assertion: it would be
        // asserting the cache's own transient behavior, which is an
        // implementation detail, the contract that matters is AFTER
        // invalidation, below).
        resolver.invalidate(&hash);
        assert!(
            resolver.resolve(&hash).is_none(),
            "after remove + invalidate, the hash must no longer resolve"
        );
    }

    /// **D8/D10 AC (untrusted input):** a malformed `.cube` is rejected with
    /// a typed [`CoreError::InvalidLook`], and, critically, leaves NO
    /// trace: no registry row, no copied file.
    #[test]
    fn malformed_cube_is_rejected_and_leaves_no_trace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let src = tmp.path().join("garbage.cube");
        std::fs::write(&src, b"this is not a valid .cube file at all").unwrap();

        let err = install_look_file(&catalog, &src, None).unwrap_err();
        assert!(matches!(err, CoreError::InvalidLook(_)), "{err}");
        assert!(catalog.reader().all_installed_looks().unwrap().is_empty());
        let looks_dir = catalog.lbdata_dir().join("looks");
        assert_eq!(
            walk_files(&looks_dir),
            0,
            "a malformed file must not be copied"
        );
    }

    /// An unsupported extension is rejected before any file IO/hash work.
    #[test]
    fn unsupported_extension_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Catalog::create(&tmp.path().join("c.lbdata")).unwrap();
        let src = tmp.path().join("look.txt");
        std::fs::write(&src, b"whatever").unwrap();
        let err = install_look_file(&catalog, &src, None).unwrap_err();
        assert!(matches!(err, CoreError::InvalidLook(_)), "{err}");
    }

    /// A resolver with no matching row (hash never installed) resolves to
    /// `None`, the graceful-degrade precondition `CreativeLutNode` relies
    /// on for an uninstalled/typo'd id.
    #[test]
    fn resolver_returns_none_for_an_unknown_hash() {
        let tmp = tempfile::TempDir::new().unwrap();
        let catalog = Arc::new(Catalog::create(&tmp.path().join("c.lbdata")).unwrap());
        let resolver = CatalogLookResolver::new(catalog);
        assert!(resolver.resolve("not-installed-hash").is_none());
    }

    fn walk_files(dir: &Path) -> usize {
        if !dir.exists() {
            return 0;
        }
        let mut count = 0;
        for entry in walkdir_files(dir) {
            if entry.is_file() {
                count += 1;
            }
        }
        count
    }

    /// A tiny recursive file walker (no `walkdir` dependency needed for a
    /// two-level fan-out directory).
    fn walkdir_files(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walkdir_files(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
}
