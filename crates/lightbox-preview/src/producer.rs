// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The T0 write path (E03 spec §3.1/§5.3, Phase B T07): turns a
//! [`crate::extract::T0Extract`] into a store file + catalog row, with
//! asset-scope dedupe on both sides.
//!
//! **Dedupe, twice over (T07 AC).**
//! 1. *Store file:* [`crate::pyramid::derive_store_key`] is a pure function
//!    of `(content_hash, scope, tier, variant)` — two calls for the same
//!    asset always land on the same relative path, so [`ensure_t0`] checks
//!    "does this file already exist" before writing (mirrors
//!    [`crate::BlobStore::put`]'s idempotence). Re-extracting an
//!    already-stored asset (a re-import, or a second virtual copy) writes
//!    **zero** new bytes to disk.
//! 2. *Catalog row:* `CatalogTxn::upsert_preview`'s conflict target is
//!    `(asset_id, tier, variant_hash) WHERE image_id IS NULL` (migration
//!    0004) — an upsert, not an insert, so calling this twice for the same
//!    asset never duplicates the row.
//!
//! Both dedupe paths are keyed on **`asset`, not `image`**: T0 is asset-scope
//! (spec §3.1), so two virtual copies (`image_a`/`image_b`) that share one
//! `asset` calling [`ensure_t0`] independently converge on the exact same
//! store file and catalog row — the second call's fast-path (below) usually
//! short-circuits before either dedupe mechanism above is even exercised.
//!
//! **Fast path.** Before touching disk or the catalog writer at all,
//! [`ensure_t0`] consults the in-RAM [`PreviewIndex`] (Phase A, T04) —
//! `best_available` is a synchronous, IO-free lookup — so a warm index
//! (the common case: index is hydrated once at session open) resolves an
//! already-built T0 without any extraction work.
//!
//! [`ensure_t1`] (Phase C, T12) is [`ensure_t0`]'s sibling for the T1
//! standard preview: same shape (index fast path → build → on-disk dedupe →
//! catalog upsert → re-index), but image-scope and variant-specific (a T1
//! row is keyed on target size/codec/quality, not just the asset) — see its
//! own doc comment for how that reshapes the fast path and source
//! selection.

use std::path::Path;
use std::sync::Mutex;

use lightbox_catalog::{Catalog, NewPreviewRow, PreviewSourceTag};
use lightbox_types::{AssetId, ContentHash, ImageId};

use crate::extract::extract_largest_embedded;
use crate::index::PreviewIndex;
use crate::pyramid::{
    derive_store_key, t0_rel_path, t1_rel_path, PreviewColorspace, PreviewDesc, PreviewScope,
    ProducerId, Tier, VariantParams,
};
use crate::store::{atomic_write, Store};
use crate::{
    config::{Codec, StandardSize},
    PreviewError,
};

/// The fixed [`VariantParams`] for every T0 row (spec §3.1: T0 has no
/// resize/quality/recipe knobs — it is the source bytes, verbatim).
/// `long_edge_px = 0` / `quality = 0` both mean "not applicable, verbatim"
/// (the spec states this convention for `long_edge_px`; `quality` follows
/// the same reading here since a byte-copy has no quality setting of its
/// own to record).
fn t0_variant_params() -> VariantParams {
    VariantParams {
        enc_ver: crate::pyramid::VARIANT_PARAMS_ENC_VER,
        producer: ProducerId::EMBEDDED,
        producer_rev: 1,
        long_edge_px: 0,
        codec: Codec::Jpeg, // T0 is always verbatim JPEG (spec §3.1)
        quality: 0,
        recipe_rev: 0, // edit-independent (spec §3.1: embedded rows never stale)
        process_version: 0,
    }
}

/// Ensures `asset`'s T0 (largest embedded preview) exists in the store +
/// catalog, extracting and writing it if this is the first call for this
/// asset, and returns its descriptor either way (spec §3.1/T07).
///
/// `image` is only used to shape the [`PreviewIndex`] fast-path lookup (T0
/// is asset-scope, but `PreviewIndex::lookup_variant` needs an `image` key
/// to search `by_image` in addition to `by_asset` — see `index.rs`'s module
/// doc comment on that deviation); the row this function writes never
/// carries an `image_id`.
///
/// **Uses `lookup_variant`, not `best_available` (Phase C bug fix).** An
/// earlier version of this function used `best_available(image, asset, 0)`
/// for both its fast path and its final return. That is wrong once a
/// higher-tier row exists for the same image/asset (T12's own
/// `producer::ensure_t1`, whose raw-source path calls this function first):
/// `best_available`'s ranking prefers higher tiers, so as soon as an image
/// has a live T1 row, `best_available(image, asset, 0)` returns the **T1**
/// descriptor even when this function is specifically asking "does T0
/// exist" — silently returning the wrong tier's `store_path`/dimensions to
/// the caller. `lookup_variant` asks for the EXACT `(tier, variant)` T0 is
/// keyed on (spec §3.1: T0 has exactly one canonical variant, so "the T0
/// row" is unambiguous), which is immune to any other tier existing.
/// Caught by `producer.rs`'s own T12 test
/// (`ensure_t1_builds_a_separate_file_per_distinct_variant`), which failed
/// before this fix; recorded in `docs/plan/epics/E03-deviations.md`, Phase
/// C.
///
/// `Err(PreviewError::NoEmbedded)` when the asset has no usable embedded
/// preview (T06) — never written to the store or catalog, never cached as a
/// negative result (a later probe of the same asset, e.g. after a sidecar
/// tool re-embeds a preview, gets a fresh chance).
pub(crate) fn ensure_t0(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    image: ImageId,
    asset: AssetId,
    content_hash: ContentHash,
    path: &Path,
) -> Result<PreviewDesc, PreviewError> {
    let variant = t0_variant_params().variant_hash();

    // Fast path: no IO at all if the index already has T0 for this asset.
    if let Some(desc) = lock(index).lookup_variant(image, asset, Tier::T0, variant) {
        return Ok(desc);
    }

    let extracted = extract_largest_embedded(path)?;
    let key = derive_store_key(content_hash, PreviewScope::Asset(asset), Tier::T0, variant);
    let rel = t0_rel_path(key);
    let abs = store.resolve(&rel);
    if !abs.exists() {
        // Content-addressed: if a file is already at this exact path, it is
        // byte-identical to `extracted.jpeg` by construction (the key is a
        // pure function of content_hash+scope+tier+variant) — no need to
        // read it back and compare, matching `BlobStore::put`'s convention.
        atomic_write(&abs, &extracted.jpeg)?;
    }
    let checksum = twox_hash::XxHash3_64::oneshot(&extracted.jpeg).to_le_bytes();

    let row = NewPreviewRow {
        asset,
        image: None, // T0 is asset-scope (spec §3.1)
        content_hash,
        tier: Tier::T0 as u8,
        variant_hash: variant.to_le_bytes(), // A-7: catalog column is LE
        source: PreviewSourceTag::Embedded,
        recipe_rev: 0,
        colorspace: match extracted.colorspace {
            crate::pyramid::PreviewColorspace::Srgb => "srgb".to_owned(),
            crate::pyramid::PreviewColorspace::TaggedIcc => "icc".to_owned(),
        },
        store_path: rel.as_str().to_owned(),
        width: extracted.width,
        height: extracted.height,
        bytes: extracted.jpeg.len() as u64,
        checksum,
    };

    let id = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(row))
        .map_err(|e| PreviewError::Catalog(e.to_string()))?;
    let full_row = catalog
        .reader()
        .preview_row(id)
        .map_err(|e| PreviewError::Catalog(e.to_string()))?
        .ok_or_else(|| {
            PreviewError::Catalog(format!("preview row {} vanished right after upsert", id.0))
        })?;

    let mut index = lock(index);
    index.upsert(&full_row);
    index
        .lookup_variant(image, asset, Tier::T0, variant)
        .ok_or_else(|| PreviewError::Catalog("preview row missing from index post-upsert".into()))
}

/// T1's [`VariantParams`] (spec §3.1/§5.1). Unlike T0's single fixed
/// variant, T1 varies by target size/codec/quality — every distinct
/// combination is its own store file + index row (spec §3.1: "Store files
/// are deduplicated by key"). `codec` is the caller's ALREADY-resolved
/// [`crate::codec::effective_codec`] tag (T11's R1 fallback), not the raw
/// request — the recorded variant and the actually-produced bytes must
/// never disagree.
fn t1_variant_params(long_edge_px: u32, codec: Codec, quality: u8) -> VariantParams {
    VariantParams {
        enc_ver: crate::pyramid::VARIANT_PARAMS_ENC_VER,
        producer: ProducerId::EMBEDDED,
        producer_rev: 1,
        long_edge_px,
        codec,
        quality,
        recipe_rev: 0, // M0: embedded-sourced, edit-independent (spec §3.1)
        process_version: 0,
    }
}

/// Resolves [`StandardSize`] to a concrete long-edge pixel target (spec
/// §5.7 `StandardSize::Auto`, T12 AC). `Fixed` is used verbatim (an
/// explicit override; the `[1280, 3840]` clamp is `Auto`'s own policy, spec
/// §5.7's phrasing: "largest display long edge, clamped 1280..=3840"
/// describes `Auto`'s resolution, not a blanket store-wide bound). `Auto`
/// itself clamps to that range; at M0 there is no live display-size feed
/// (E08's UI wires one at a later epic — Q3's own default posture is "no
/// proactive rebuild" once a real hint exists), so it resolves to the
/// clamp's upper bound — the exact value T10's own AC exercises ("T1 built
/// from the embedded JPEG at 3840px long edge").
pub(crate) fn resolve_standard_long_edge(standard: StandardSize) -> u32 {
    match standard {
        StandardSize::Auto => 3840,
        StandardSize::Fixed(px) => px,
    }
}

/// Ensures `image`'s T1 (standard, display-sized preview) exists in the
/// store + catalog for the given size/codec/quality policy, building it if
/// this is the first call for this exact variant (spec §3.1/§5.3, T12).
/// Mirrors [`ensure_t0`]'s shape (index fast path → build → on-disk dedupe
/// → catalog upsert → re-index) with two differences forced by T1 being
/// image-scope AND variant-specific rather than asset-scope-with-one-fixed-
/// variant: the fast path is an EXACT `(tier, variant)` lookup
/// ([`PreviewIndex::lookup_variant`], not `best_available`'s ranked pick —
/// `best_available` could return a *different* T1 variant that happens to
/// outrank the one being asked for), and the store key/row are image-scope.
///
/// **Source selection (T12 AC).** A raw asset's T1 downscales the
/// already-stored T0 (calling [`ensure_t0`] first — memoized, so this never
/// re-extracts); a non-raw **JPEG** source builds T1 directly from the
/// original file and writes **no T0 row** for it (T12 AC's explicit "no T0
/// row" case; spec §3.5's M0 registration text: "for non-raw JPEG/TIFF
/// sources: no T0 — T1 built by downscaling the source file itself"). A
/// TIFF/PNG source with no embedded JPEG surrogate fails the same way T0
/// extraction already does ([`PreviewError::NoEmbedded`]) — full-pixel
/// non-JPEG decode is out of this M0 producer's scope (E02/E05).
///
/// **Orientation is NOT baked into the stored bytes** — same convention as
/// T0 (`extract::T0Extract`'s `width`/`height` are the as-stored SOF dims,
/// not display-upright ones). `decode.rs`'s `decode_for_display` already
/// bakes orientation uniformly for every tier at read time (T08); baking it
/// again here would double-rotate the 90°/270° family. This is safe because
/// "long edge" (the resize target) is transpose-invariant: resizing before
/// or after a rotation clamps to the same effective displayed size either
/// way.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // Phase D's scheduler (PreviewService::request(Tier::T1, ..))
                    // is the production caller; exercised directly by this phase's own tests
                    // today (T12), mirroring `ng/sched/mod.rs`'s "real ... registration is
                    // [later phase] wiring" precedent elsewhere in this workspace.
pub(crate) fn ensure_t1(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    image: ImageId,
    asset: AssetId,
    content_hash: ContentHash,
    path: &Path,
    standard_px: StandardSize,
    codec_pref: Codec,
    quality: u8,
) -> Result<PreviewDesc, PreviewError> {
    let long_edge_px = resolve_standard_long_edge(standard_px);
    let codec = crate::codec::effective_codec(codec_pref);
    let params = t1_variant_params(long_edge_px, codec, quality);
    let variant = params.variant_hash();

    // Fast path: this EXACT variant already exists — no IO at all.
    if let Some(desc) = lock(index).lookup_variant(image, asset, Tier::T1, variant) {
        return Ok(desc);
    }

    let probe = lightbox_decode::probe(path)?;
    let (rgb_px, src_w, src_h, colorspace) =
        if matches!(probe.format, lightbox_decode::ProbedFormat::Jpeg) {
            // Non-raw JPEG source: build directly, no T0 row (T12 AC).
            let bytes = std::fs::read(path)?;
            let colorspace = crate::extract::sniff_colorspace(&bytes);
            let (px, w, h) = crate::pipeline::decode_jpeg_rgb(&bytes)?;
            (px, w, h, colorspace)
        } else {
            let t0 = ensure_t0(store, catalog, index, image, asset, content_hash, path)?;
            let bytes = std::fs::read(store.resolve(&t0.store_path))?;
            let (px, w, h) = crate::pipeline::decode_jpeg_rgb(&bytes)?;
            (px, w, h, t0.colorspace)
        };

    let (rgb_px, w, h) =
        crate::pipeline::resize_rgb_lanczos3_to_fit(rgb_px, src_w, src_h, long_edge_px)?;
    let encoded = crate::codec::resolve(codec)
        .encode(
            &crate::codec::RgbImage {
                px: rgb_px,
                width: w,
                height: h,
            },
            quality,
        )
        .map_err(|e| PreviewError::Encode(e.to_string()))?;

    let key = derive_store_key(content_hash, PreviewScope::Image(image), Tier::T1, variant);
    let rel = t1_rel_path(key, codec);
    let abs = store.resolve(&rel);
    if !abs.exists() {
        // Content-addressed, same reasoning as `ensure_t0`: a file already
        // at this exact key is byte-identical to `encoded` by construction.
        atomic_write(&abs, &encoded)?;
    }
    let checksum = twox_hash::XxHash3_64::oneshot(&encoded).to_le_bytes();

    let row = NewPreviewRow {
        asset,
        image: Some(image), // T1 is image-scope (spec §3.1)
        content_hash,
        tier: Tier::T1 as u8,
        variant_hash: variant.to_le_bytes(), // A-7: catalog column is LE
        source: PreviewSourceTag::Embedded,
        recipe_rev: 0,
        colorspace: match colorspace {
            PreviewColorspace::Srgb => "srgb".to_owned(),
            PreviewColorspace::TaggedIcc => "icc".to_owned(),
        },
        store_path: rel.as_str().to_owned(),
        width: w,
        height: h,
        bytes: encoded.len() as u64,
        checksum,
    };

    let id = catalog
        .writer()
        .with_txn(move |txn| txn.upsert_preview(row))
        .map_err(|e| PreviewError::Catalog(e.to_string()))?;
    let full_row = catalog
        .reader()
        .preview_row(id)
        .map_err(|e| PreviewError::Catalog(e.to_string()))?
        .ok_or_else(|| {
            PreviewError::Catalog(format!("preview row {} vanished right after upsert", id.0))
        })?;

    let mut idx = lock(index);
    idx.upsert(&full_row);
    idx.lookup_variant(image, asset, Tier::T1, variant)
        .ok_or_else(|| PreviewError::Catalog("preview row missing from index post-upsert".into()))
}

fn lock(index: &Mutex<PreviewIndex>) -> std::sync::MutexGuard<'_, PreviewIndex> {
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PreviewStoreConfig;
    use lightbox_types::Orientation;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        assert!(
            dir.join("manifest.toml").exists(),
            "fixture corpus missing at {} — run `cargo xtask fixtures` first",
            dir.display()
        );
        dir
    }

    /// A fresh store + catalog + hydrated (empty) index, all rooted in one
    /// tempdir — the fixture every T07 test shares.
    struct Harness {
        _dir: tempfile::TempDir,
        store: Store,
        catalog: Catalog,
        index: Mutex<PreviewIndex>,
    }

    fn harness() -> Harness {
        let dir = tempfile::TempDir::new().unwrap();
        let store =
            Store::open(&PreviewStoreConfig::with_defaults(dir.path().to_path_buf())).unwrap();
        let catalog = Catalog::create(&dir.path().join("t.lbdata")).unwrap();
        let index = Mutex::new(PreviewIndex::load(&catalog.reader()).unwrap());
        Harness {
            _dir: dir,
            store,
            catalog,
            index,
        }
    }

    /// Inserts one asset (with a real content hash of `path`'s bytes) and
    /// returns its `AssetId` — enough scaffolding for `ensure_t0`, without
    /// pulling in a real folder/import flow.
    fn insert_asset(catalog: &Catalog, filename: &str, content_hash: ContentHash) -> AssetId {
        // A synthetic root path is enough: these tests pass the real fixture
        // path to `ensure_t0` directly, never resolving it back through the
        // catalog (`asset_abs_path`), so nothing here needs to exist on disk.
        let root = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_root(None, Path::new("/synthetic-root")))
            .unwrap();
        let folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(root, None, "shoot"))
            .unwrap();
        catalog
            .writer()
            .with_txn({
                let filename = filename.to_owned();
                move |txn| {
                    let batch = vec![lightbox_catalog::NewAsset {
                        folder,
                        filename,
                        content_hash,
                        format: "CR2".to_owned(),
                        camera_make: None,
                        camera_model: None,
                        capture_time: None,
                        width: 100,
                        height: 100,
                        orientation: Orientation::O1,
                        bytes: 10,
                        mtime_utc: None,
                        decode_error: None,
                        import_session: None,
                    }];
                    Ok(txn.insert_assets(&batch)?.inserted[0])
                }
            })
            .unwrap()
    }

    /// T07 AC: extracting T0 writes the expected store file + a
    /// `source='embedded'` index row at the §3.1 key scheme path.
    #[test]
    fn ensure_t0_writes_the_store_file_and_index_row() {
        let h = harness();
        let content_hash = ContentHash([3; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let image = insert_default_image(&h.catalog, asset);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let desc = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(desc.tier, Tier::T0);
        assert_eq!(desc.source, crate::pyramid::PreviewSource::Embedded);
        assert_eq!((desc.width, desc.height), (3456, 2304));
        assert!(desc.store_path.as_str().starts_with("previews/"));
        assert!(desc.store_path.as_str().ends_with(".t0.jpg"));
        let abs = h.store.resolve(&desc.store_path);
        assert!(abs.is_file(), "T0 file must exist on disk");
        assert!(std::fs::metadata(&abs).unwrap().len() > 0);
    }

    /// T07 AC: re-import of an identical file (same asset, called again)
    /// creates ZERO new store files.
    #[test]
    fn reextraction_of_the_same_asset_writes_no_new_files() {
        let h = harness();
        let content_hash = ContentHash([4; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let first = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        let preview_dir = h.store.root().join("previews");
        let count_before = count_files(&preview_dir);
        assert_eq!(count_before, 1, "exactly one T0 file after the first call");

        let second = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(first.store_path, second.store_path);
        assert_eq!(count_files(&preview_dir), count_before, "no new files");

        // Drop the RAM index's memory of this and force the on-disk-check
        // path too (simulates a cold-index re-import racing a warm file).
        h.index.lock().unwrap().remove(
            h.catalog
                .reader()
                .preview_lookup(asset, None, Tier::T0 as u8, first.variant.to_le_bytes())
                .unwrap()
                .unwrap()
                .id,
        );
        let third = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(first.store_path, third.store_path);
        assert_eq!(
            count_files(&preview_dir),
            count_before,
            "still no new files"
        );
    }

    /// T07 AC: two virtual copies over one asset share one T0 row/file.
    #[test]
    fn virtual_copies_of_one_asset_share_one_t0_row_and_file() {
        let h = harness();
        let content_hash = ContentHash([5; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let copy_a = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(10),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        let copy_b = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(11),
            asset,
            content_hash,
            &path,
        )
        .unwrap();
        assert_eq!(copy_a.store_path, copy_b.store_path);
        assert_eq!(copy_a.variant, copy_b.variant);
        assert_eq!(count_files(&h.store.root().join("previews")), 1);
        assert_eq!(h.index.lock().unwrap().len(), 1, "one row, not two");
    }

    /// T06/T07: a raw with no usable embedded preview never writes a file or
    /// row, and reports the typed `NoEmbedded` error.
    #[test]
    fn no_embedded_preview_writes_nothing() {
        let h = harness();
        let content_hash = ContentHash([6; 16]);
        let asset = insert_asset(&h.catalog, "sigma-fp.dng", content_hash);
        let path = fixtures_dir().join("sigma-fp.dng");

        let err = ensure_t0(
            &h.store,
            &h.catalog,
            &h.index,
            ImageId(1),
            asset,
            content_hash,
            &path,
        )
        .unwrap_err();
        assert!(matches!(err, PreviewError::NoEmbedded), "{err:?}");
        assert!(
            !h.store.root().join("previews").join("00").exists() || {
                count_files(&h.store.root().join("previews")) == 0
            }
        );
        assert_eq!(h.index.lock().unwrap().len(), 0);
    }

    fn count_files(root: &Path) -> usize {
        fn walk(p: &Path, n: &mut usize) {
            let Ok(entries) = std::fs::read_dir(p) else {
                return;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(&path, n);
                } else {
                    *n += 1;
                }
            }
        }
        let mut n = 0;
        walk(root, &mut n);
        n
    }

    // ── T12: the T1 build pipeline ──────────────────────────────────────
    //
    // T1 is image-scope (spec §3.1): `NewPreviewRow{image: Some(_), ..}`'s
    // `image_id` is a real FOREIGN KEY (migration 0004), unlike T0's
    // asset-scope `image: None` — every T12 test below inserts a real
    // `image` row via `insert_default_image`, not just an asset.

    /// Inserts the default (non-virtual-copy) image row for `asset` and
    /// returns its `ImageId` (mirrors `index.rs`'s own test helper pattern).
    fn insert_default_image(catalog: &Catalog, asset: AssetId) -> ImageId {
        catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap()
    }

    /// T12 AC: `request(T1)` on a raw fixture yields a `.t1` file + index
    /// row (`source='embedded'`, correct `variant_hash`) — and, per T12's
    /// "mirrors `ensure_t0`" shape, a T0 row/file too (downscaling a raw's
    /// T1 goes through the stored T0, spec §3.1: "At M0, built by
    /// downscaling the embedded JPEG").
    #[test]
    fn ensure_t1_on_a_raw_fixture_writes_the_t1_file_and_index_row() {
        let h = harness();
        let content_hash = ContentHash([10; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let image = insert_default_image(&h.catalog, asset);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let desc = ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            StandardSize::Fixed(1600),
            Codec::Jpeg,
            90,
        )
        .unwrap();

        assert_eq!(desc.tier, Tier::T1);
        assert_eq!(desc.source, crate::pyramid::PreviewSource::Embedded);
        assert_eq!(desc.recipe_rev, 0);
        assert!(!desc.stale);
        // canon-eos-350d.cr2's embedded preview is 3456x2304 (T07's own
        // test); downscaled to fit a 1600 long edge: 1600x1067.
        assert_eq!((desc.width, desc.height), (1600, 1067));
        assert!(desc.store_path.as_str().starts_with("previews/"));
        assert!(
            desc.store_path.as_str().ends_with(".t1.jpg"),
            "{}",
            desc.store_path.as_str()
        );
        let abs = h.store.resolve(&desc.store_path);
        assert!(abs.is_file(), "T1 file must exist on disk");
        assert!(std::fs::metadata(&abs).unwrap().len() > 0);

        // The exact variant this call built is retrievable by exact lookup
        // (not just "some T1 row exists").
        let params = t1_variant_params(1600, Codec::Jpeg, 90);
        let found = h
            .index
            .lock()
            .unwrap()
            .lookup_variant(image, asset, Tier::T1, params.variant_hash())
            .unwrap();
        assert_eq!(found.store_path, desc.store_path);

        // T12 "mirrors ensure_t0": a raw source's T1 goes through T0, so a
        // T0 row/file exists too.
        let t0 = h
            .index
            .lock()
            .unwrap()
            .lookup_variant(image, asset, Tier::T0, t0_variant_params().variant_hash())
            .unwrap();
        assert!(h.store.resolve(&t0.store_path).is_file());
    }

    /// T12 AC: a non-raw JPEG source builds T1 directly from the source
    /// file — no T0 row.
    #[test]
    fn ensure_t1_on_a_non_raw_jpeg_source_builds_directly_with_no_t0_row() {
        let h = harness();
        let content_hash = ContentHash([11; 16]);
        let asset = insert_asset(&h.catalog, "lightbox-tiny.jpg", content_hash);
        let image = insert_default_image(&h.catalog, asset);
        let path = fixtures_dir().join("lightbox-tiny.jpg");

        let desc = ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            StandardSize::Fixed(1600), // far bigger than the 16x16 source: never upscales
            Codec::Jpeg,
            90,
        )
        .unwrap();

        assert_eq!(desc.tier, Tier::T1);
        assert_eq!((desc.width, desc.height), (16, 16), "never upscales");
        let abs = h.store.resolve(&desc.store_path);
        assert!(abs.is_file());

        // No T0 row/file at all: only the T1 file exists under previews/.
        assert_eq!(
            count_files(&h.store.root().join("previews")),
            1,
            "exactly one file — the T1 — no T0"
        );
        let no_t0 = h.index.lock().unwrap().lookup_variant(
            image,
            asset,
            Tier::T0,
            t0_variant_params().variant_hash(),
        );
        assert!(no_t0.is_none(), "no T0 row for a non-raw JPEG source");
    }

    /// T12: the fast path. Calling `ensure_t1` twice for the exact same
    /// variant writes zero new files/rows the second time (mirrors T07's
    /// `reextraction_of_the_same_asset_writes_no_new_files`).
    #[test]
    fn ensure_t1_is_idempotent_for_the_same_variant() {
        let h = harness();
        let content_hash = ContentHash([12; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let image = insert_default_image(&h.catalog, asset);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let first = ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            StandardSize::Fixed(1600),
            Codec::Jpeg,
            90,
        )
        .unwrap();
        let preview_dir = h.store.root().join("previews");
        // T0 + T1: exactly two files after the first call.
        let count_before = count_files(&preview_dir);
        assert_eq!(count_before, 2);

        let second = ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            StandardSize::Fixed(1600),
            Codec::Jpeg,
            90,
        )
        .unwrap();
        assert_eq!(first.store_path, second.store_path);
        assert_eq!(count_files(&preview_dir), count_before, "no new files");
    }

    /// T12: a DIFFERENT variant (different target size) of the same image
    /// builds a SEPARATE T1 file/row rather than colliding with or
    /// replacing the first — `lookup_variant`'s whole reason for existing
    /// over `best_available` in this producer.
    #[test]
    fn ensure_t1_builds_a_separate_file_per_distinct_variant() {
        let h = harness();
        let content_hash = ContentHash([13; 16]);
        let asset = insert_asset(&h.catalog, "canon-eos-350d.cr2", content_hash);
        let image = insert_default_image(&h.catalog, asset);
        let path = fixtures_dir().join("canon-eos-350d.cr2");

        let small = ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            StandardSize::Fixed(1280),
            Codec::Jpeg,
            90,
        )
        .unwrap();
        let large = ensure_t1(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            &path,
            StandardSize::Fixed(2000),
            Codec::Jpeg,
            90,
        )
        .unwrap();

        assert_ne!(small.store_path, large.store_path);
        assert_eq!((small.width.max(small.height)), 1280);
        assert_eq!((large.width.max(large.height)), 2000);
        // T0 (shared) + two distinct T1 variants = 3 files.
        assert_eq!(count_files(&h.store.root().join("previews")), 3);
    }

    /// T12 / spec §5.7: `StandardSize::Auto` resolves to the clamp's upper
    /// bound (3840) at M0 (no live display-size feed yet — see
    /// `resolve_standard_long_edge`'s doc comment); `Fixed` is used
    /// verbatim.
    #[test]
    fn resolve_standard_long_edge_auto_is_3840_fixed_is_verbatim() {
        assert_eq!(resolve_standard_long_edge(StandardSize::Auto), 3840);
        assert_eq!(resolve_standard_long_edge(StandardSize::Fixed(1234)), 1234);
    }
}
