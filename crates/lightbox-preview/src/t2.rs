// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The T2 tiled preview store (E03 spec §4.3/§3.1, Phase F T20): tile
//! addressing over the `t2_tile_px` grid, per-tile atomic write,
//! `manifest.cbor` (grid geometry + a presence bitmap, temp+rename), and
//! aggregate-bytes accounting.
//!
//! **Producer.** At M0 there is no real T2 producer — E05 (M1+) implements
//! [`Produced::Tiles`] for real, honoring the recipe. This module ships a
//! SYNTHETIC producer ([`synthetic_tiles`]) so T20's own tests (and any
//! later phase's integration tests) can exercise the write/read/manifest
//! path end-to-end without a renderer, matching the spec's own framing
//! ("exercised at E03 time by a synthetic producer in tests", §1.1 item 2).
//! [`ensure_t2_synthetic`] mirrors `producer::ensure_t0`/`ensure_t1`'s shape
//! (build → per-tile atomic write → manifest → catalog upsert → re-index)
//! rather than routing through a generic `PreviewProducer` trait — the same
//! documented precedent Phases B-E used (B-11/C-11/D-14/E-11): a second,
//! *real* producer (E05) is what would justify that abstraction, and this
//! phase still doesn't have one.
//!
//! **Manifest format.** `previews/<hh>/<key>.t2/manifest.cbor` — fixed-order
//! CBOR (`ciborium`, the same wire format `pyramid.rs`'s `VariantParams`
//! uses), written via the same `store::atomic_write` temp+rename primitive
//! every other store file uses (T02). The presence bitmap is one `bool` per
//! grid cell in row-major order — a tile absent from the bitmap (or with
//! `false`) is `Ok(None)` from [`t2_tile_read`] without ever touching disk
//! for it, satisfying T20's "missing tile → `Ok(None)`" AC directly from the
//! manifest rather than a per-tile existence probe.

use std::path::Path;
use std::sync::Mutex;

use lightbox_catalog::{Catalog, NewPreviewRow, PreviewSourceTag};
use lightbox_types::{AssetId, ContentHash, ImageId};

use crate::codec::RgbImage;
use crate::config::Codec;
use crate::index::PreviewIndex;
use crate::pyramid::{
    derive_store_key, t2_rel_dir, PreviewDesc, PreviewScope, ProducerId, RelPath, Tier,
    VariantParams,
};
use crate::store::{atomic_write, Store};
use crate::PreviewError;

/// A tile's grid coordinate (spec §5.2 `PreviewService::t2_tile`).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct TileCoord {
    pub x: u32,
    pub y: u32,
}

/// The T2 grid geometry (spec §4.3): `image_w × image_h` tiled at `tile_px`
/// (edge tiles crop rather than pad — [`TileGrid::tile_dims`]).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct TileGrid {
    pub image_w: u32,
    pub image_h: u32,
    pub tile_px: u32,
    pub cols: u32,
    pub rows: u32,
}

impl TileGrid {
    pub fn new(image_w: u32, image_h: u32, tile_px: u32) -> TileGrid {
        let tile_px = tile_px.max(1);
        let cols = image_w.div_ceil(tile_px).max(1);
        let rows = image_h.div_ceil(tile_px).max(1);
        TileGrid {
            image_w,
            image_h,
            tile_px,
            cols,
            rows,
        }
    }

    pub fn tile_count(&self) -> u32 {
        self.cols * self.rows
    }

    /// The pixel size of the tile at `coord` — full `tile_px²` except the
    /// last row/column, which crops to the image's actual remaining extent.
    pub fn tile_dims(&self, coord: TileCoord) -> (u32, u32) {
        let w = if coord.x + 1 == self.cols {
            self.image_w - coord.x * self.tile_px
        } else {
            self.tile_px
        };
        let h = if coord.y + 1 == self.rows {
            self.image_h - coord.y * self.tile_px
        } else {
            self.tile_px
        };
        (w.max(1), h.max(1))
    }

    fn index_of(&self, coord: TileCoord) -> Option<usize> {
        if coord.x >= self.cols || coord.y >= self.rows {
            return None;
        }
        Some((coord.y * self.cols + coord.x) as usize)
    }

    #[allow(dead_code)] // exercised by `synthetic_tiles`/tests today; a real
                        // E05 producer (M1+) or a future manifest-driven
                        // iteration helper is the production caller.
    fn coords(&self) -> impl Iterator<Item = TileCoord> + '_ {
        (0..self.rows).flat_map(move |y| (0..self.cols).map(move |x| TileCoord { x, y }))
    }
}

/// What a (real or synthetic) T2 producer hands back (spec §5.3
/// `Produced::Tiles`, narrowed to this crate's `RgbImage` boundary type —
/// same B-3/C-9 reasoning as `codec.rs`'s own `RgbImage`; the full
/// `Produced`/`PreviewProducer` trait is not built, see the module doc
/// comment).
#[allow(dead_code)] // constructed by `synthetic_tiles`/tests today; E05's
                    // real producer (M1+) is the production caller.
pub(crate) enum Produced {
    Tiles {
        grid: TileGrid,
        tiles: Vec<(TileCoord, RgbImage)>,
    },
}

/// A SYNTHETIC T2 producer (T20 AC: "synthetic producer for tests"). Fills
/// every tile with a deterministic, coordinate-derived solid color — enough
/// to prove round-trip correctness (distinct, checkable content per tile)
/// without a real renderer.
#[allow(dead_code)] // exercised by this module's own tests today; a live
                    // caller needs a real T2-requesting entry point
                    // (`PreviewService::request(Tier::T2, ..)`), which does not
                    // exist until E05 registers a real producer (M1+) — same
                    // precedent as `producer.rs`'s C-9 deviation.
pub(crate) fn synthetic_tiles(grid: TileGrid) -> Produced {
    let mut tiles = Vec::with_capacity(grid.tile_count() as usize);
    for coord in grid.coords() {
        let (w, h) = grid.tile_dims(coord);
        let color = [
            ((coord.x.wrapping_mul(37)) % 251) as u8,
            ((coord.y.wrapping_mul(53)) % 251) as u8,
            (((coord.x + coord.y).wrapping_mul(17)) % 251) as u8,
        ];
        let mut px = Vec::with_capacity((w as usize) * (h as usize) * 3);
        for _ in 0..(w as usize * h as usize) {
            px.extend_from_slice(&color);
        }
        tiles.push((
            coord,
            RgbImage {
                px,
                width: w,
                height: h,
            },
        ));
    }
    Produced::Tiles { grid, tiles }
}

/// One decoded manifest / on-disk `manifest.cbor` payload. Fixed-field CBOR
/// tuple (mirrors `VariantParams::canonical_bytes`'s "hand-written, not
/// derived-struct" discipline) so the wire format never silently drifts with
/// a struct-field reorder.
#[derive(Clone, Debug, PartialEq)]
struct Manifest {
    format: u16,
    image_w: u32,
    image_h: u32,
    tile_px: u32,
    cols: u32,
    rows: u32,
    codec: u8,
    /// Row-major, one entry per grid cell.
    present: Vec<bool>,
}

const MANIFEST_FORMAT: u16 = 1;

impl Manifest {
    #[allow(dead_code)] // reachable only via `write_t2`, itself test-only in
                        // a production (non-test) build today — see that
                        // function's own doc comment.
    fn to_cbor(&self) -> Vec<u8> {
        #[derive(serde::Serialize)]
        struct Tuple<'a>(u16, u32, u32, u32, u32, u32, u8, &'a [bool]);
        let tuple = Tuple(
            self.format,
            self.image_w,
            self.image_h,
            self.tile_px,
            self.cols,
            self.rows,
            self.codec,
            &self.present,
        );
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&tuple, &mut buf)
            .expect("in-memory CBOR encode of a fixed tuple is infallible");
        buf
    }

    fn from_cbor(bytes: &[u8]) -> Result<Manifest, String> {
        #[derive(serde::Deserialize)]
        struct Tuple(u16, u32, u32, u32, u32, u32, u8, Vec<bool>);
        let Tuple(format, image_w, image_h, tile_px, cols, rows, codec, present) =
            ciborium::de::from_reader(bytes).map_err(|e| format!("manifest.cbor: {e}"))?;
        if format != MANIFEST_FORMAT {
            return Err(format!("unsupported manifest format {format}"));
        }
        Ok(Manifest {
            format,
            image_w,
            image_h,
            tile_px,
            cols,
            rows,
            codec,
            present,
        })
    }

    fn grid(&self) -> TileGrid {
        TileGrid {
            image_w: self.image_w,
            image_h: self.image_h,
            tile_px: self.tile_px,
            cols: self.cols,
            rows: self.rows,
        }
    }
}

/// `<t2_dir>/manifest.cbor` (spec §7 T20).
pub(crate) fn manifest_rel_path(t2_dir: &RelPath) -> RelPath {
    RelPath(format!("{}/manifest.cbor", t2_dir.as_str()))
}

/// `<t2_dir>/<x>_<y>.<ext>` (spec §3.1: `previews/<hh>/<key>.t2/<x>_<y>.jxl`).
fn tile_rel_path(t2_dir: &RelPath, coord: TileCoord, codec: Codec) -> RelPath {
    RelPath(format!(
        "{}/{}_{}.{}",
        t2_dir.as_str(),
        coord.x,
        coord.y,
        codec.file_extension()
    ))
}

/// One decoded T2 tile read (spec §5.2 `PreviewService::t2_tile`'s
/// `EncodedTile`). Still-encoded bytes — the caller decodes via
/// [`crate::codec`], mirroring how T1's stored bytes travel.
#[derive(Clone, Debug)]
pub struct EncodedTile {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
}

/// T2's [`VariantParams`] (spec §3.1: `long_edge_px = 0` — native 1:1,
/// mirrors T0's "0 = not applicable" convention).
#[allow(dead_code)] // reachable only via `write_t2` today, same reasoning.
fn t2_variant_params(codec: Codec, quality: u8, producer_rev: u32) -> VariantParams {
    VariantParams {
        enc_ver: crate::pyramid::VARIANT_PARAMS_ENC_VER,
        producer: ProducerId("synthetic-t2-test"),
        producer_rev,
        long_edge_px: 0,
        codec,
        quality,
        recipe_rev: 0,
        process_version: 0,
    }
}

/// Writes a (real or synthetic) [`Produced::Tiles`] result to the store:
/// per-tile atomic write (only for tiles `present` includes — spec T20 AC
/// "write/read full + partial tile sets"), `manifest.cbor` (temp+rename,
/// covering EVERY grid cell's presence, not just the ones this call wrote),
/// aggregate-bytes accounting, and a `tier=2` catalog upsert/re-index
/// (mirrors `producer::ensure_t0`/`ensure_t1`'s shape).
///
/// T2 rows are always `source = 'rendered'` (spec §3.1's tier table: T2
/// "reflects edits? yes (rendered only)") — even this synthetic producer's
/// output is tagged that way, since the catalog schema has no third source
/// value and "synthetic-test-content" is closer to "a producer rendered
/// SOMETHING" than to "verbatim/downscaled from the camera's embedded JPEG"
/// (the `embedded` tag's actual meaning).
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // exercised by this module's own tests today; the
                    // production caller is `PreviewService::request(Tier::T2, ..)`
                    // once E05 registers a real producer (M1+) — same C-9 precedent.
pub(crate) fn write_t2(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    image: ImageId,
    asset: AssetId,
    content_hash: ContentHash,
    produced: Produced,
    codec: Codec,
    quality: u8,
    producer_rev: u32,
) -> Result<PreviewDesc, PreviewError> {
    let Produced::Tiles { grid, tiles } = produced;
    let params = t2_variant_params(codec, quality, producer_rev);
    let variant = params.variant_hash();
    let key = derive_store_key(content_hash, PreviewScope::Image(image), Tier::T2, variant);
    let t2_dir = t2_rel_dir(key);

    let encoder = crate::codec::resolve(codec);
    let mut present = vec![false; (grid.cols * grid.rows) as usize];
    let mut aggregate_bytes: u64 = 0;
    for (coord, rgb) in &tiles {
        let Some(idx) = grid.index_of(*coord) else {
            continue; // out-of-grid coordinate from a misbehaving producer: skip, not panic
        };
        let encoded = encoder
            .encode(rgb, quality)
            .map_err(|e| PreviewError::Encode(e.to_string()))?;
        let rel = tile_rel_path(&t2_dir, *coord, codec);
        let abs = store.resolve(&rel);
        atomic_write(&abs, &encoded)?;
        aggregate_bytes += encoded.len() as u64;
        present[idx] = true;
    }

    let manifest = Manifest {
        format: MANIFEST_FORMAT,
        image_w: grid.image_w,
        image_h: grid.image_h,
        tile_px: grid.tile_px,
        cols: grid.cols,
        rows: grid.rows,
        codec: codec as u8,
        present,
    };
    let manifest_bytes = manifest.to_cbor();
    let manifest_rel = manifest_rel_path(&t2_dir);
    atomic_write(&store.resolve(&manifest_rel), &manifest_bytes)?;
    aggregate_bytes += manifest_bytes.len() as u64;
    let checksum = twox_hash::XxHash3_64::oneshot(&manifest_bytes).to_le_bytes();

    let row = NewPreviewRow {
        asset,
        image: Some(image),
        content_hash,
        tier: Tier::T2 as u8,
        variant_hash: variant.to_le_bytes(),
        source: PreviewSourceTag::Rendered,
        recipe_rev: 0,
        colorspace: "srgb".to_owned(),
        store_path: t2_dir.as_str().to_owned(),
        width: grid.image_w,
        height: grid.image_h,
        bytes: aggregate_bytes,
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
    let mut idx = index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    idx.upsert(&full_row);
    idx.lookup_variant(image, asset, Tier::T2, variant)
        .ok_or_else(|| PreviewError::Catalog("preview row missing from index post-upsert".into()))
}

/// Convenience: builds a full synthetic tile set (every cell present) and
/// writes it (T20's straightforward "the happy path" entry point; the
/// partial-set case calls [`write_t2`] with a filtered `tiles` list
/// directly, see the test module).
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)] // exercised by this module's and `evict.rs`'s tests today.
pub(crate) fn ensure_t2_synthetic(
    store: &Store,
    catalog: &Catalog,
    index: &Mutex<PreviewIndex>,
    image: ImageId,
    asset: AssetId,
    content_hash: ContentHash,
    image_w: u32,
    image_h: u32,
    tile_px: u32,
) -> Result<PreviewDesc, PreviewError> {
    let grid = TileGrid::new(image_w, image_h, tile_px);
    let produced = synthetic_tiles(grid);
    write_t2(
        store,
        catalog,
        index,
        image,
        asset,
        content_hash,
        produced,
        crate::codec::effective_codec(Codec::Jpeg),
        90,
        1,
    )
}

/// Reads one tile (spec §5.2 `PreviewService::t2_tile`). `desc.store_path`
/// is the `.t2` DIRECTORY (not a file — see [`write_t2`]). `Ok(None)`
/// whenever the manifest says the tile is absent, the manifest itself is
/// missing (a not-yet-built or reconcile-worthy T2 row), or the tile file
/// diverges from what the manifest claims (self-healing: the caller treats
/// this exactly like "not built yet" and can re-enqueue, spec §3.2's
/// "caches are disposable" posture — never a hard error for a divergence
/// this store can recover from by rebuilding).
pub(crate) fn t2_tile_read(
    store: &Store,
    t2_dir: &RelPath,
    coord: TileCoord,
) -> Result<Option<EncodedTile>, PreviewError> {
    let manifest_abs = store.resolve(&manifest_rel_path(t2_dir));
    let manifest_bytes = match std::fs::read(&manifest_abs) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(PreviewError::from(e)),
    };
    let manifest = match Manifest::from_cbor(&manifest_bytes) {
        Ok(m) => m,
        Err(_) => return Ok(None), // torn/foreign manifest: reconcile territory, not a hard error
    };
    let grid = manifest.grid();
    let Some(idx) = grid.index_of(coord) else {
        return Ok(None);
    };
    if !manifest.present.get(idx).copied().unwrap_or(false) {
        return Ok(None);
    }
    let codec = if manifest.codec == Codec::Jxl as u8 {
        Codec::Jxl
    } else {
        Codec::Jpeg
    };
    let rel = tile_rel_path(t2_dir, coord, codec);
    match std::fs::read(store.resolve(&rel)) {
        Ok(bytes) => {
            let (w, h) = grid.tile_dims(coord);
            Ok(Some(EncodedTile {
                bytes,
                width: w,
                height: h,
                codec,
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PreviewError::from(e)),
    }
}

/// Sum of every tile file's size + the manifest's, read fresh from disk
/// (diagnostics/tests — the catalog row's `bytes` column is the accounted
/// value `write_t2` computed at build time; this is an independent
/// cross-check).
#[allow(dead_code)] // exercised by tests; a future verify_store(Full) checksum
                    // spot-check is a natural production caller.
pub(crate) fn t2_dir_bytes_on_disk(store: &Store, t2_dir: &RelPath) -> u64 {
    let root = store.resolve(t2_dir);
    walk_bytes(&root)
}

fn walk_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += walk_bytes(&path);
        } else {
            total += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PreviewStoreConfig;
    use lightbox_types::Orientation;

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

    fn insert_image(catalog: &Catalog, content_hash: ContentHash) -> (AssetId, ImageId) {
        let root = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_root(None, Path::new("/synthetic-root")))
            .unwrap();
        let folder = catalog
            .writer()
            .with_txn(move |txn| txn.upsert_folder(root, None, "shoot"))
            .unwrap();
        let asset = catalog
            .writer()
            .with_txn(move |txn| {
                let batch = vec![lightbox_catalog::NewAsset {
                    folder,
                    filename: "synthetic.dng".to_owned(),
                    content_hash,
                    format: "DNG".to_owned(),
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
            })
            .unwrap();
        let image = catalog
            .writer()
            .with_txn(move |txn| Ok(txn.insert_default_images(&[asset])?[0]))
            .unwrap();
        (asset, image)
    }

    #[test]
    fn tile_grid_geometry_crops_edge_tiles() {
        let grid = TileGrid::new(600, 500, 256);
        assert_eq!((grid.cols, grid.rows), (3, 2));
        assert_eq!(grid.tile_count(), 6);
        assert_eq!(grid.tile_dims(TileCoord { x: 0, y: 0 }), (256, 256));
        assert_eq!(grid.tile_dims(TileCoord { x: 2, y: 0 }), (600 - 512, 256));
        assert_eq!(
            grid.tile_dims(TileCoord { x: 2, y: 1 }),
            (600 - 512, 500 - 256)
        );
    }

    /// T20 AC: a 100 MP synthetic image — write/read a FULL tile set, and
    /// prove the `Produced::Tiles` path end-to-end (real per-tile atomic
    /// writes + manifest + catalog row + index re-hydration).
    #[test]
    fn write_and_read_a_full_100mp_tile_set() {
        let h = harness();
        let content_hash = ContentHash([21; 16]);
        let (asset, image) = insert_image(&h.catalog, content_hash);
        // 12000x8500 ~= 102 MP at 256px tiles -> 47*34 = 1598 tiles.
        let (w, hh) = (12000u32, 8500u32);

        let desc = ensure_t2_synthetic(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            w,
            hh,
            256,
        )
        .unwrap();
        assert_eq!(desc.tier, Tier::T2);
        assert_eq!((desc.width, desc.height), (w, hh));
        assert!(desc.store_path.as_str().ends_with(".t2"));

        let t2_dir = desc.store_path.clone();
        let grid = TileGrid::new(w, hh, 256);
        assert_eq!(grid.tile_count(), 47 * 34);

        // Spot-check a handful of tiles across the grid (corner + interior +
        // the cropped last row/col) round-trip through the real JPEG codec.
        for coord in [
            TileCoord { x: 0, y: 0 },
            TileCoord { x: 10, y: 5 },
            TileCoord {
                x: grid.cols - 1,
                y: 0,
            },
            TileCoord {
                x: 0,
                y: grid.rows - 1,
            },
            TileCoord {
                x: grid.cols - 1,
                y: grid.rows - 1,
            },
        ] {
            let tile = t2_tile_read(&h.store, &t2_dir, coord).unwrap().unwrap();
            assert!(!tile.bytes.is_empty());
            let (ew, eh) = grid.tile_dims(coord);
            assert_eq!((tile.width, tile.height), (ew, eh));
        }

        // A coordinate outside the grid is a clean `Ok(None)`, not a panic.
        let out_of_range = t2_tile_read(&h.store, &t2_dir, TileCoord { x: 9999, y: 9999 }).unwrap();
        assert!(out_of_range.is_none());

        assert_eq!(h.index.lock().unwrap().len(), 1);
    }

    /// T20 AC: a partial tile set (some tiles never produced) — the missing
    /// ones read back as `Ok(None)`, present ones round-trip.
    #[test]
    fn partial_tile_set_missing_tiles_read_as_none() {
        let h = harness();
        let content_hash = ContentHash([22; 16]);
        let (asset, image) = insert_image(&h.catalog, content_hash);
        let grid = TileGrid::new(1000, 800, 256);
        let Produced::Tiles { tiles, .. } = synthetic_tiles(grid);
        // Keep only even-indexed tiles — a genuinely partial producer run
        // (e.g. a cancelled/interrupted E05 tile build).
        let partial: Vec<_> = tiles
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, t)| t)
            .collect();
        let kept_count = partial.len();

        let desc = write_t2(
            &h.store,
            &h.catalog,
            &h.index,
            image,
            asset,
            content_hash,
            Produced::Tiles {
                grid,
                tiles: partial,
            },
            Codec::Jpeg,
            90,
            1,
        )
        .unwrap();
        let t2_dir = desc.store_path.clone();

        let mut present = 0;
        let mut absent = 0;
        for coord in grid.coords() {
            match t2_tile_read(&h.store, &t2_dir, coord).unwrap() {
                Some(_) => present += 1,
                None => absent += 1,
            }
        }
        assert_eq!(present, kept_count);
        assert_eq!(absent, (grid.tile_count() as usize) - kept_count);
        assert!(
            absent > 0,
            "this test is only meaningful if some are missing"
        );
    }

    /// A T2 row for a store with no manifest at all (e.g. the row exists but
    /// the directory was never built, or was wiped) is `Ok(None)` for any
    /// tile — never a hard error.
    #[test]
    fn missing_manifest_reads_as_none_not_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let store =
            Store::open(&PreviewStoreConfig::with_defaults(dir.path().to_path_buf())).unwrap();
        let t2_dir = RelPath("previews/ab/doesnotexist.t2".to_owned());
        let got = t2_tile_read(&store, &t2_dir, TileCoord { x: 0, y: 0 }).unwrap();
        assert!(got.is_none());
    }
}
