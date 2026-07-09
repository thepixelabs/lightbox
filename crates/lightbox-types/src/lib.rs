// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-types` — shared vocabulary types for the Lightbox workspace.
//!
//! Owned by **E01** (spec §3.1). This crate exists to break the dependency
//! cycle between `lightbox-catalog` and `lightbox-core` on shared id/error
//! types without letting leaf crates depend on `lightbox-core`. It contains
//! **no behavior** and no dependencies beyond `serde`.
//!
//! Frozen surface: the types below are consumed across crate boundaries by
//! every other epic; changes after M0 require a joint review (spec §8.4).

/// Catalog rowid behind a newtype; never a raw `i64` across a crate boundary.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct AssetId(pub i64);

/// Rowid of an `image` row (a rendition of an asset; virtual copies are E07).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImageId(pub i64);

/// Rowid of a `folder` row.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct FolderId(pub i64);

/// Rowid of a `library_root` row.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct RootId(pub i64);

/// Rowid of an `import_session` row.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct ImportSessionId(pub i64);

/// Rowid of a `mask` row. **Content is owned by E12**; `Recipe.masks` (E09)
/// carries only ordered `MaskId` refs (spec §3.1 ownership rule). Additive per
/// E09 T1 — see `docs/plan/epics/E09-deviations.md` A-1 (E01 joint-review flag).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct MaskId(pub i64);

/// Rowid of a `retouch_op` row. Content owned by E12/E14; `Recipe.retouch` (E09)
/// carries only ordered `RetouchOpId` refs. Additive per E09 T1 (deviations A-1).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct RetouchOpId(pub i64);

/// Rowid of a `snapshot` row (a named, self-contained edit projection, spec §3.1).
/// Additive per E09 T1 (deviations A-1).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct SnapshotId(pub i64);

/// Rowid of a `history_step` row (the persistent edit step log, spec §3.1).
/// Additive per E09 T1 (deviations A-1).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct HistoryStepId(pub i64);

/// Rowid of a `preview` row (one built pyramid tier/variant, E03 spec §4).
/// Additive per E03 Phase A (T03/T04) — the preview index's row identity.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct PreviewId(pub i64);

/// Rowid of a `raw_cache_entry` row (one accounted raw-cache container, E03
/// spec §4/§5.4). Additive per E03 Phase E (T18) — the raw-cache
/// accounting/LRU index's row identity, mirroring [`PreviewId`]'s pattern.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct RawCacheEntryId(pub i64);

/// xxh3-128 of the full original file. Keys caches + relink (architecture §3.1).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContentHash(pub [u8; 16]);

impl ContentHash {
    /// Lowercase 32-char hex encoding (stable across platforms).
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(32);
        for b in self.0 {
            use std::fmt::Write;
            // Writing to a String cannot fail.
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// Parses a 32-char hex string (case-insensitive). `None` on any other input.
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 || !s.is_ascii() {
            return None;
        }
        let bytes = s.as_bytes();
        let mut out = [0u8; 16];
        for (i, chunk) in bytes.chunks_exact(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16)?;
            let lo = (chunk[1] as char).to_digit(16)?;
            out[i] = ((hi << 4) | lo) as u8;
        }
        Some(ContentHash(out))
    }
}

/// EXIF orientation 1..=8. Applied in `DisplayTransformNode`, never baked into
/// stored pixels (spec §3.1).
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Orientation {
    /// Row 0 = top, column 0 = left (the "normal" orientation).
    #[default]
    O1,
    /// Mirrored horizontally.
    O2,
    /// Rotated 180°.
    O3,
    /// Mirrored vertically.
    O4,
    /// Mirrored horizontally, then rotated 270° CW.
    O5,
    /// Rotated 90° CW.
    O6,
    /// Mirrored horizontally, then rotated 90° CW.
    O7,
    /// Rotated 270° CW.
    O8,
}

impl Orientation {
    /// Maps an EXIF orientation value (1..=8) to the enum. `None` otherwise.
    pub fn from_exif(v: u16) -> Option<Self> {
        Some(match v {
            1 => Orientation::O1,
            2 => Orientation::O2,
            3 => Orientation::O3,
            4 => Orientation::O4,
            5 => Orientation::O5,
            6 => Orientation::O6,
            7 => Orientation::O7,
            8 => Orientation::O8,
            _ => return None,
        })
    }

    /// The EXIF orientation value (1..=8) — what the catalog stores.
    pub fn exif_value(self) -> u16 {
        match self {
            Orientation::O1 => 1,
            Orientation::O2 => 2,
            Orientation::O3 => 3,
            Orientation::O4 => 4,
            Orientation::O5 => 5,
            Orientation::O6 => 6,
            Orientation::O7 => 7,
            Orientation::O8 => 8,
        }
    }

    /// True when the orientation swaps width and height (90°/270° family).
    pub fn transposes(self) -> bool {
        matches!(
            self,
            Orientation::O5 | Orientation::O6 | Orientation::O7 | Orientation::O8
        )
    }
}

/// Pipeline process version (architecture §4.5). Old PVs stay renderable forever.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProcessVersion(pub u16);

/// The M0 process version — the only one that exists during E01.
pub const PV_M0: ProcessVersion = ProcessVersion(1);

/// Which develop surface a source can expose (architecture §2.4): raw
/// sources get the full toolset; rendered sources get the same pipeline
/// minus the raw-only stages (hidden, not disabled). Derived from the
/// probe (`lightbox_decode::ProbedFormat::source_kind`); carried on every
/// working-set item (E04 spec §4.1); consumed by E08's panel gating and
/// E02/E05's pipeline assembly.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
#[non_exhaustive] // headroom: a video-frame source is a v1.x possibility
pub enum SourceKind {
    /// Mosaic camera raw (CR2/CR3/NEF/ARW/RAF/ORF/DNG/…).
    Raw,
    /// Already-rendered pixels (JPEG/TIFF/PNG/HEIC/…).
    Rendered,
}

/// Which pipeline tier produced a set of pixels (spec §3.5/§3.6).
///
/// Shared vocabulary between the pixels-in seam (`lightbox-render`'s
/// `SourceImage`) and the preview seam (`lightbox-preview`'s `DecodedImage`).
/// M0 knows only the camera's embedded JPEG; **E03** adds the on-disk pyramid
/// tiers and **E02+E05** the raw-decode path — hence `#[non_exhaustive]`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum SourceTier {
    /// The camera's embedded JPEG preview.
    EmbeddedPreview,
}

/// Pick/reject flag on an `image` row. Stored as -1/0/+1 in the catalog.
#[derive(
    Copy, Clone, PartialEq, Eq, Hash, Debug, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Flag {
    /// No flag (catalog value `0`).
    #[default]
    None,
    /// Picked (catalog value `1`).
    Pick,
    /// Rejected (catalog value `-1`).
    Reject,
}

impl Flag {
    /// Catalog encoding: -1 reject / 0 none / 1 pick (migration `0001`).
    pub fn to_db(self) -> i64 {
        match self {
            Flag::None => 0,
            Flag::Pick => 1,
            Flag::Reject => -1,
        }
    }

    /// Inverse of [`Flag::to_db`]. `None` for values the schema forbids.
    pub fn from_db(v: i64) -> Option<Self> {
        Some(match v {
            0 => Flag::None,
            1 => Flag::Pick,
            -1 => Flag::Reject,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn id_newtypes_round_trip_serde() {
        // T1 acceptance criterion: id newtypes round-trip serde.
        let asset = AssetId(42);
        let json = serde_json::to_string(&asset).unwrap();
        assert_eq!(json, "42");
        assert_eq!(serde_json::from_str::<AssetId>(&json).unwrap(), asset);

        let image = ImageId(-7);
        assert_eq!(
            serde_json::from_str::<ImageId>(&serde_json::to_string(&image).unwrap()).unwrap(),
            image
        );
        let folder = FolderId(i64::MAX);
        assert_eq!(
            serde_json::from_str::<FolderId>(&serde_json::to_string(&folder).unwrap()).unwrap(),
            folder
        );
        let root = RootId(1);
        assert_eq!(
            serde_json::from_str::<RootId>(&serde_json::to_string(&root).unwrap()).unwrap(),
            root
        );
        let session = ImportSessionId(0);
        assert_eq!(
            serde_json::from_str::<ImportSessionId>(&serde_json::to_string(&session).unwrap())
                .unwrap(),
            session
        );
    }

    #[test]
    fn e09_id_newtypes_round_trip_serde() {
        // E09 T1: additive newtypes serialize as their inner i64 (transparent).
        let mask = MaskId(7);
        assert_eq!(serde_json::to_string(&mask).unwrap(), "7");
        assert_eq!(serde_json::from_str::<MaskId>("7").unwrap(), mask);

        let retouch = RetouchOpId(-3);
        assert_eq!(
            serde_json::from_str::<RetouchOpId>(&serde_json::to_string(&retouch).unwrap()).unwrap(),
            retouch
        );
        let snap = SnapshotId(i64::MAX);
        assert_eq!(
            serde_json::from_str::<SnapshotId>(&serde_json::to_string(&snap).unwrap()).unwrap(),
            snap
        );
        let step = HistoryStepId(0);
        assert_eq!(
            serde_json::from_str::<HistoryStepId>(&serde_json::to_string(&step).unwrap()).unwrap(),
            step
        );
        // E03 Phase A (T03/T04): PreviewId follows the same additive-newtype pattern.
        let preview = PreviewId(42);
        assert_eq!(
            serde_json::from_str::<PreviewId>(&serde_json::to_string(&preview).unwrap()).unwrap(),
            preview
        );
        // E03 Phase E (T18): RawCacheEntryId follows the same pattern.
        let rawcache = RawCacheEntryId(43);
        assert_eq!(
            serde_json::from_str::<RawCacheEntryId>(&serde_json::to_string(&rawcache).unwrap())
                .unwrap(),
            rawcache
        );

        // Ord is derived (used by ordered id lists / future keying).
        assert!(MaskId(1) < MaskId(2));
        assert!(HistoryStepId(10) > HistoryStepId(9));
        assert!(PreviewId(1) < PreviewId(2));
        assert!(RawCacheEntryId(1) < RawCacheEntryId(2));
    }

    #[test]
    fn content_hash_hex_known_values() {
        let h = ContentHash([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0xff,
        ]);
        assert_eq!(h.to_hex(), "000102030405060708090a0b0c0d0eff");
        assert_eq!(
            ContentHash::from_hex("000102030405060708090a0b0c0d0eff"),
            Some(h)
        );
        // Case-insensitive parse.
        assert_eq!(
            ContentHash::from_hex("000102030405060708090A0B0C0D0EFF"),
            Some(h)
        );
    }

    #[test]
    fn content_hash_from_hex_rejects_bad_input() {
        assert_eq!(ContentHash::from_hex(""), None);
        assert_eq!(ContentHash::from_hex("00"), None); // too short
        assert_eq!(ContentHash::from_hex(&"0".repeat(33)), None); // too long
        assert_eq!(ContentHash::from_hex(&"g".repeat(32)), None); // non-hex
        assert_eq!(ContentHash::from_hex(&"é".repeat(16)), None); // non-ascii, len() == 32 bytes
    }

    proptest! {
        // §6 property test: ContentHash hex round-trip (PR-blocking).
        #[test]
        fn content_hash_hex_round_trip(bytes in proptest::array::uniform16(any::<u8>())) {
            let h = ContentHash(bytes);
            let hex = h.to_hex();
            prop_assert_eq!(hex.len(), 32);
            prop_assert_eq!(ContentHash::from_hex(&hex), Some(h));
            prop_assert_eq!(ContentHash::from_hex(&hex.to_uppercase()), Some(h));
        }
    }

    #[test]
    fn orientation_exif_round_trip() {
        for v in 1..=8u16 {
            let o = Orientation::from_exif(v).unwrap();
            assert_eq!(o.exif_value(), v);
        }
        assert_eq!(Orientation::from_exif(0), None);
        assert_eq!(Orientation::from_exif(9), None);
        assert!(Orientation::O6.transposes());
        assert!(!Orientation::O3.transposes());
        assert_eq!(Orientation::default(), Orientation::O1);
    }

    #[test]
    fn flag_db_round_trip() {
        for f in [Flag::None, Flag::Pick, Flag::Reject] {
            assert_eq!(Flag::from_db(f.to_db()), Some(f));
        }
        assert_eq!(Flag::from_db(2), None);
        assert_eq!(Flag::default(), Flag::None);
    }

    #[test]
    fn source_kind_serde_round_trips() {
        for kind in [SourceKind::Raw, SourceKind::Rendered] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<SourceKind>(&json).unwrap(), kind);
        }
        assert_ne!(SourceKind::Raw, SourceKind::Rendered);
    }

    #[test]
    fn process_version_constant() {
        assert_eq!(PV_M0, ProcessVersion(1));
        let json = serde_json::to_string(&PV_M0).unwrap();
        assert_eq!(
            serde_json::from_str::<ProcessVersion>(&json).unwrap(),
            PV_M0
        );
    }
}
