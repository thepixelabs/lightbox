// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `.lbdata` cache store: layout, manifest, atomic blob IO, and the
//! generic content-addressed [`BlobStore`] (E03 spec §3.2/§5.5, Phase A
//! T01/T02).
//!
//! **Seam note:** the spec's §5.2 `PreviewService::open` is Phase D's
//! facade (T14) — it composes the index, scheduler, and decoded LRU that
//! don't exist yet. [`Store::open`] is the Phase-A-owned primitive
//! underneath it: everything T01's acceptance criterion actually asks for
//! ("creates the §3.2 layout + `store.toml`") happens here. Recorded as a
//! deviation in `docs/plan/epics/E03-deviations.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::PreviewStoreConfig;
use crate::error::StoreError;
use crate::PreviewError;

/// The store manifest (`store.toml`, spec §3.2).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StoreManifest {
    pub format: u32,
    pub store_uuid: String,
    pub created_by: String,
    /// The store's previous root, set by a completed relocation (Phase F,
    /// T21). `None` for a store that has never moved.
    pub relocated_from: Option<String>,
}

/// The manifest format version this build writes/understands (spec §3.2:
/// `format=1`).
pub const STORE_FORMAT_VERSION: u32 = 1;

/// Reserved cache-surface directories created at open (spec §3.2). Not
/// exhaustive of the whole `.lbdata` tree — `catalog.sqlite`/`backups/` are
/// E01's, `thumbcache.sqlite` is Phase F's (T22), `rawcache/` fan-out dirs
/// are created lazily by Phase E (T17) the same way `previews/` is here.
const RESERVED_DIRS: &[&str] = &["previews", "rawcache", "smartpreview", "masks"];

/// The `.lbdata` cache store (spec §3.2). Owns the manifest and the root
/// path; blob IO (this module) and the preview index (`index.rs`) are built
/// on top of it.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    manifest: StoreManifest,
}

impl Store {
    /// Opens (or, on an empty/fresh dir, creates) the store: the reserved
    /// directories + `store.toml` (spec §3.2). Idempotent — reopening an
    /// existing store reads the manifest back rather than overwriting it.
    ///
    /// Refuses a manifest whose `format` is newer than this build supports
    /// (forward-only, matching the catalog's own `SchemaTooNew` posture).
    pub fn open(cfg: &PreviewStoreConfig) -> Result<Store, PreviewError> {
        std::fs::create_dir_all(&cfg.root)?;
        for dir in RESERVED_DIRS {
            std::fs::create_dir_all(cfg.root.join(dir))?;
        }

        let manifest_path = cfg.root.join("store.toml");
        let manifest = if manifest_path.exists() {
            let text = std::fs::read_to_string(&manifest_path)?;
            let manifest: StoreManifest = toml::from_str(&text)
                .map_err(|e| PreviewError::Manifest(format!("store.toml: {e}")))?;
            if manifest.format > STORE_FORMAT_VERSION {
                return Err(PreviewError::StoreTooNew {
                    found: manifest.format,
                    supported: STORE_FORMAT_VERSION,
                });
            }
            manifest
        } else {
            let manifest = StoreManifest {
                format: STORE_FORMAT_VERSION,
                store_uuid: uuid::Uuid::new_v4().to_string(),
                created_by: format!("lightbox-preview/{}", env!("CARGO_PKG_VERSION")),
                relocated_from: None,
            };
            write_manifest_atomic(&manifest_path, &manifest)?;
            manifest
        };

        Ok(Store {
            root: cfg.root.clone(),
            manifest,
        })
    }

    /// The store root (== the catalog's `.lbdata` directory, spec §3.2).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn manifest(&self) -> &StoreManifest {
        &self.manifest
    }

    /// A [`BlobStore`] scoped to one reserved namespace (spec §5.5).
    pub fn blob_store(&self, ns: BlobNamespace) -> BlobStore {
        BlobStore {
            root: self.root.join(ns.dir_name()),
        }
    }

    /// The absolute path a fan-out-relative store path resolves to (used by
    /// preview/rawcache producers in later phases; Phase A exposes it since
    /// `Store` owns `root`).
    pub fn resolve(&self, rel: &crate::pyramid::RelPath) -> PathBuf {
        rel.to_path_buf(&self.root)
    }
}

fn write_manifest_atomic(path: &Path, manifest: &StoreManifest) -> Result<(), PreviewError> {
    let text = toml::to_string_pretty(manifest)
        .map_err(|e| PreviewError::Manifest(format!("encoding store.toml: {e}")))?;
    atomic_write(path, text.as_bytes())?;
    Ok(())
}

/// Reserved [`BlobStore`] namespaces (spec §3.2/§5.5). `Masks` is E14's baked
/// AI-segmentation rasters; `SmartPreview` is reserved for E16 — E03 ships
/// the directory and the mechanism, not the feature (spec §1.2 non-goals).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum BlobNamespace {
    Masks,
    SmartPreview,
}

impl BlobNamespace {
    fn dir_name(self) -> &'static str {
        match self {
            BlobNamespace::Masks => "masks",
            BlobNamespace::SmartPreview => "smartpreview",
        }
    }
}

/// xxh3-128 content address of a [`BlobStore`] entry (spec §5.5).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct BlobRef(pub [u8; 16]);

impl BlobRef {
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(32);
        for b in self.0 {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    fn of(bytes: &[u8]) -> BlobRef {
        BlobRef(twox_hash::XxHash3_128::oneshot(bytes).to_be_bytes())
    }
}

/// A generic content-addressed blob namespace inside `.lbdata` (spec §5.5).
/// Blob file names are the bare hex content address (no extension) — the
/// primitive is content-type-agnostic; a domain owner (e.g. E14's `.png`
/// masks) interprets the bytes, it does not need the filename to say so.
///
/// **Not LRU-evicted** (spec §5.5): namespaces under this mechanism hold
/// authoritative content a later epic owns the lifecycle of, never
/// E03-owned cache state.
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// Content-addressed, idempotent: writing the same bytes twice is a
    /// no-op the second time (spec §5.5 AC).
    pub fn put(&self, bytes: &[u8]) -> Result<BlobRef, StoreError> {
        let r = BlobRef::of(bytes);
        let path = self.blob_path(r);
        if path.exists() {
            return Ok(r);
        }
        atomic_write(&path, bytes)?;
        Ok(r)
    }

    /// `Ok(None)` on a missing blob. A checksum mismatch (torn/corrupted
    /// file) removes the entry and reports [`StoreError::Corrupt`] — never a
    /// silent wrong-content return.
    pub fn get(&self, r: &BlobRef) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.blob_path(*r);
        match std::fs::read(&path) {
            Ok(bytes) => {
                if BlobRef::of(&bytes) != *r {
                    let _ = std::fs::remove_file(&path);
                    return Err(StoreError::Corrupt(r.to_hex()));
                }
                Ok(Some(bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// `Ok(true)` if a file was actually removed, `Ok(false)` if it was
    /// already absent (idempotent).
    pub fn remove(&self, r: &BlobRef) -> Result<bool, StoreError> {
        match std::fs::remove_file(self.blob_path(*r)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    fn blob_path(&self, r: BlobRef) -> PathBuf {
        let hex = r.to_hex();
        self.root.join(&hex[..2]).join(hex)
    }
}

/// Monotonic counter so concurrent atomic writes in the same process never
/// pick the same temp filename (pid alone is not enough: many threads share
/// one pid).
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Temp-file-in-same-directory → fsync → rename (spec §3.2: "A crash at any
/// point leaves either the old state or the new state, never a torn visible
/// file"). Creates the parent directory (fan-out dirs are lazy, spec §3.1's
/// `<hh>` layout).
///
/// Best-effort parent-directory fsync on Unix so the rename's directory
/// entry is itself durable, not just the file content — `fsync`ing a
/// directory has no equivalent on Windows (NTFS's own metadata journal
/// covers the rename atomicity guarantee this function relies on there), so
/// it's skipped rather than faked (spec Risk R6: exotic filesystems are
/// best-effort, checksums make torn files self-healing regardless).
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .expect("store paths are always nested under a namespace directory");
    std::fs::create_dir_all(parent)?;
    let tmp_name = format!(
        ".tmp-{}-{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let tmp_path = parent.join(tmp_name);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    let rename_result = std::fs::rename(&tmp_path, path);
    if rename_result.is_err() {
        let _ = std::fs::remove_file(&tmp_path); // never leave a temp file behind
    }
    rename_result?;
    #[cfg(unix)]
    {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> PreviewStoreConfig {
        PreviewStoreConfig::with_defaults(dir.to_path_buf())
    }

    /// T01 AC: opening an empty dir creates the §3.2 layout + `store.toml`.
    #[test]
    fn open_on_empty_dir_creates_layout_and_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&cfg(dir.path())).unwrap();
        for name in RESERVED_DIRS {
            assert!(dir.path().join(name).is_dir(), "missing {name}");
        }
        assert!(dir.path().join("store.toml").is_file());
        assert_eq!(store.manifest().format, STORE_FORMAT_VERSION);
        assert!(!store.manifest().store_uuid.is_empty());
        assert_eq!(store.manifest().relocated_from, None);
    }

    /// Reopening reads the same manifest back rather than minting a new
    /// `store_uuid` (identity must be stable across process restarts).
    #[test]
    fn reopen_is_idempotent_and_preserves_identity() {
        let dir = tempfile::TempDir::new().unwrap();
        let first = Store::open(&cfg(dir.path())).unwrap();
        let second = Store::open(&cfg(dir.path())).unwrap();
        assert_eq!(first.manifest().store_uuid, second.manifest().store_uuid);
    }

    /// A store manifest from a newer build is refused, not silently adopted.
    #[test]
    fn future_format_version_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        let manifest = StoreManifest {
            format: STORE_FORMAT_VERSION + 1,
            store_uuid: "x".to_owned(),
            created_by: "test".to_owned(),
            relocated_from: None,
        };
        std::fs::write(
            dir.path().join("store.toml"),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let err = Store::open(&cfg(dir.path())).unwrap_err();
        assert!(matches!(err, PreviewError::StoreTooNew { .. }), "{err:?}");
    }

    #[test]
    fn blob_store_put_get_remove_round_trip_and_put_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&cfg(dir.path())).unwrap();
        let blobs = store.blob_store(BlobNamespace::Masks);

        let r1 = blobs.put(b"hello mask bytes").unwrap();
        let r2 = blobs.put(b"hello mask bytes").unwrap(); // idempotent
        assert_eq!(r1, r2);

        let got = blobs.get(&r1).unwrap().unwrap();
        assert_eq!(got, b"hello mask bytes");

        assert!(blobs.remove(&r1).unwrap());
        assert_eq!(blobs.get(&r1).unwrap(), None);
        assert!(
            !blobs.remove(&r1).unwrap(),
            "second remove is a no-op, not an error"
        );
    }

    #[test]
    fn blob_store_fanout_uses_first_hex_byte() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&cfg(dir.path())).unwrap();
        let blobs = store.blob_store(BlobNamespace::Masks);
        let r = blobs.put(b"fanout probe").unwrap();
        let hex = r.to_hex();
        let expected = dir.path().join("masks").join(&hex[..2]).join(&hex);
        assert!(expected.is_file(), "{}", expected.display());
    }

    #[test]
    fn get_detects_a_torn_file_drops_it_and_reports_corrupt() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&cfg(dir.path())).unwrap();
        let blobs = store.blob_store(BlobNamespace::SmartPreview);
        let r = blobs.put(b"original content").unwrap();

        // Simulate a torn write: truncate the file in place.
        let hex = r.to_hex();
        let path = dir.path().join("smartpreview").join(&hex[..2]).join(&hex);
        std::fs::write(&path, b"short").unwrap();

        let err = blobs.get(&r).unwrap_err();
        assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
        assert!(
            !path.exists(),
            "a corrupt blob must be dropped, not left behind"
        );
    }

    #[test]
    fn no_temp_files_survive_a_successful_write() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&cfg(dir.path())).unwrap();
        let blobs = store.blob_store(BlobNamespace::Masks);
        for i in 0..20u32 {
            blobs.put(format!("payload {i}").as_bytes()).unwrap();
        }
        let mut leftover_tmp = 0;
        for entry in walk(&dir.path().join("masks")) {
            if entry
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".tmp-"))
            {
                leftover_tmp += 1;
            }
        }
        assert_eq!(leftover_tmp, 0);
    }

    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }
}
