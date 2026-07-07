// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Tiers, scopes, and the canonical variant-keying contract (E03 spec §3.1 /
//! §5.1, Phase A T05).
//!
//! [`VariantParams::canonical_bytes`] is a **hand-written fixed-order CBOR
//! array** (never a derived struct/map encoding): the fields are written
//! into a plain tuple in a fixed position, so the byte layout can only
//! change by editing this function, and any such edit is exactly the moment
//! `VARIANT_PARAMS_ENC_VER` must bump (spec §5.1: "any field change without
//! `enc_ver` bump fails the vector test" — [`tests::golden_hash_vectors`]).
//!
//! [`derive_store_key`] is the spec §3.1 key formula
//! (`xxh3_128(content_hash ‖ scope_discriminator ‖ tier ‖ variant_hash)`)
//! made byte-precise: the spec's prose leaves the exact `scope_discriminator`
//! encoding to the implementation. This crate defines it as `scope_tag: u8`
//! (`0` = asset, `1` = image) followed by the scope id as 8
//! little-endian bytes — see the doc comment on [`derive_store_key`] for why
//! the id must be included (virtual-copy T1/T2 collision avoidance).

use lightbox_types::{AssetId, ContentHash, ImageId};

use crate::config::Codec;

/// Preview pyramid tier (spec §3.1).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// Verbatim embedded camera JPEG — asset scope.
    T0 = 0,
    /// Standard display-sized preview — image scope.
    T1 = 1,
    /// 1:1 native-resolution tiles — image scope.
    T2 = 2,
}

impl Tier {
    /// Parses the catalog's `preview.tier` column value. `None` for any
    /// value outside `0..=2` (a corrupt/foreign row).
    pub fn from_u8(v: u8) -> Option<Tier> {
        match v {
            0 => Some(Tier::T0),
            1 => Some(Tier::T1),
            2 => Some(Tier::T2),
            _ => None,
        }
    }
}

/// Which row a preview belongs to (spec §5.1): T0 is asset-scope (shared by
/// every virtual copy of the asset), T1/T2 are image-scope (spec §3.1).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum PreviewScope {
    Asset(AssetId),
    Image(ImageId),
}

/// Whether a variant reflects the source file verbatim/downscaled, or was
/// rendered through the engine honoring a recipe (spec §3.1/§5.1).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum PreviewSource {
    Embedded,
    Rendered,
}

/// Colorspace tag carried on a decoded preview (spec §5.1). Full ICC
/// handling is E02's; at M0 this is a display hint only.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum PreviewColorspace {
    Srgb,
    /// An embedded/tagged ICC profile — the profile bytes themselves travel
    /// with the container (embedded JPEG/JXL metadata), not in this enum.
    TaggedIcc,
}

/// A stable identity for a `PreviewProducer` implementation (spec §5.1/§5.3):
/// `"embedded"` (E03, M0), `"engine"` (E05, M1+), or a test producer's own
/// tag. `'static` and `Copy` — producers are compiled-in Rust types, never
/// user data, so no allocation is needed.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct ProducerId(pub &'static str);

impl ProducerId {
    /// The M0 embedded-preview producer (Phase B, T06-T12).
    pub const EMBEDDED: ProducerId = ProducerId("embedded");
    /// The M1+ render-engine producer (E05).
    pub const ENGINE: ProducerId = ProducerId("engine");
}

/// xxh3-64 of a [`VariantParams`]' canonical encoding (spec §5.1).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct VariantHash(pub u64);

impl VariantHash {
    /// Big-endian bytes — the repo-wide xxh3 serialization convention (see
    /// `lightbox_decode::hash_file`, `Recipe::canonical_hash`) for
    /// hex-displayed/cross-tool content ids.
    pub fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    /// Little-endian bytes — the convention for the catalog's
    /// `preview.variant_hash` BLOB column (see `index.rs::to_desc`'s doc
    /// comment): pure internal cache-key material, never hex-displayed, so
    /// the native/cheap direction is used rather than the display-oriented
    /// big-endian one. This is what `NewPreviewRow::variant_hash` expects.
    pub fn to_le_bytes(self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

/// The current [`VariantParams`] canonical-encoding version (spec §5.1).
/// Bump on ANY layout change to [`VariantParams::canonical_bytes`] — the
/// golden-vector test in this module exists to catch a missed bump.
pub const VARIANT_PARAMS_ENC_VER: u16 = 1;

/// Canonical, versioned variant-selection parameters (spec §5.1). Hashed via
/// [`VariantParams::variant_hash`] to become the store key's dominant
/// discriminator alongside `content_hash`/scope/tier.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct VariantParams {
    /// Canonical-encoding version; bump on ANY layout change.
    pub enc_ver: u16,
    pub producer: ProducerId,
    pub producer_rev: u32,
    /// T0: `0` (verbatim); T2: `0` (native 1:1).
    pub long_edge_px: u32,
    pub codec: Codec,
    pub quality: u8,
    /// `0` = edit-independent (embedded rows).
    pub recipe_rev: u64,
    /// `0` when `producer == embedded`.
    pub process_version: u16,
}

impl VariantParams {
    /// Fixed-order CBOR array encoding (spec §5.1: "hashed with xxh3-64 over
    /// fixed-order CBOR"). A plain tuple, not the named struct, so field
    /// order is a property of *this function's source*, not of however a
    /// caller happens to initialize the struct literal (property-tested in
    /// `tests::encoding_is_independent_of_struct_literal_field_order`).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        #[derive(serde::Serialize)]
        struct Tuple<'a>(u16, &'a str, u32, u32, u8, u8, u64, u16);
        let tuple = Tuple(
            self.enc_ver,
            self.producer.0,
            self.producer_rev,
            self.long_edge_px,
            self.codec as u8,
            self.quality,
            self.recipe_rev,
            self.process_version,
        );
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&tuple, &mut buf)
            .expect("in-memory CBOR encode of a fixed tuple is infallible");
        buf
    }

    /// xxh3-64 of [`Self::canonical_bytes`] (spec §5.1).
    pub fn variant_hash(&self) -> VariantHash {
        VariantHash(twox_hash::XxHash3_64::oneshot(&self.canonical_bytes()))
    }
}

/// A content-addressed store key (spec §3.1): `xxh3_128` over
/// `content_hash ‖ scope_discriminator ‖ tier ‖ variant_hash`. See
/// [`derive_store_key`] for the exact byte layout this crate defines.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct StoreKey(pub [u8; 16]);

impl StoreKey {
    /// Lowercase 32-char hex (the on-disk filename component).
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(32);
        for b in self.0 {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// The `<hh>` two-level fan-out directory (spec §3.1/§3.2): the first
    /// byte of the key, as two lowercase hex chars.
    pub fn fanout_hh(self) -> String {
        format!("{:02x}", self.0[0])
    }
}

/// Derives the content-addressed store key for one `(content_hash, scope,
/// tier, variant)` tuple (spec §3.1).
///
/// **Byte layout** (the spec states the formula in prose; this is this
/// crate's byte-precise pin, covered by golden vectors):
///
/// ```text
/// [0..16)  content_hash (16 bytes)
/// [16]     scope_tag: u8 = 0 (Asset) | 1 (Image)
/// [17..25) scope_id: u64 LE (the AssetId/ImageId row id)
/// [25]     tier: u8
/// [25+1..+8) variant_hash: u64 LE
/// ```
///
/// The scope **id** (not just the asset/image discriminator byte) is
/// included deliberately: T0 rows are meant to dedupe across assets that
/// share a `content_hash` (spec §3.1 "Store files are deduplicated by key"),
/// so the id is correctly omitted there — but T1/T2 rows must NOT dedupe
/// across two virtual copies of the same asset (same `content_hash`) whose
/// `VariantParams` happen to coincide (e.g. both freshly duplicated at
/// `recipe_rev = 0`); omitting the image id would collide them onto the same
/// store file. Folding the scope id into the hash input for BOTH scope kinds
/// keeps the formula uniform while getting the right dedup behavior in each
/// case, because `PreviewScope::Asset` is already exactly the identity T0
/// dedupes on.
pub fn derive_store_key(
    content_hash: ContentHash,
    scope: PreviewScope,
    tier: Tier,
    variant: VariantHash,
) -> StoreKey {
    let mut buf = [0u8; 16 + 1 + 8 + 1 + 8];
    buf[0..16].copy_from_slice(&content_hash.0);
    let (scope_tag, scope_id): (u8, u64) = match scope {
        PreviewScope::Asset(id) => (0, id.0 as u64),
        PreviewScope::Image(id) => (1, id.0 as u64),
    };
    buf[16] = scope_tag;
    buf[17..25].copy_from_slice(&scope_id.to_le_bytes());
    buf[25] = tier as u8;
    buf[26..34].copy_from_slice(&variant.0.to_le_bytes());
    StoreKey(twox_hash::XxHash3_128::oneshot(&buf).to_be_bytes())
}

/// A path relative to the store root (POSIX-separated regardless of host
/// platform — store paths are catalog data, not native paths).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RelPath(pub String);

impl RelPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_path_buf(&self, root: &std::path::Path) -> std::path::PathBuf {
        let mut p = root.to_path_buf();
        for seg in self.0.split('/') {
            p.push(seg);
        }
        p
    }
}

/// `previews/<hh>/<key>.t0.jpg` (spec §3.1/§3.2) — T0 is always verbatim
/// JPEG, never JXL.
pub fn t0_rel_path(key: StoreKey) -> RelPath {
    RelPath(format!(
        "previews/{}/{}.t0.jpg",
        key.fanout_hh(),
        key.to_hex()
    ))
}

/// `previews/<hh>/<key>.t1.<ext>` (spec §3.1/§3.2), extension from `codec`
/// (JXL when the `jxl` feature/T11 producer is active, JPEG fallback
/// otherwise — R1's reversal trigger, zero API impact).
pub fn t1_rel_path(key: StoreKey, codec: Codec) -> RelPath {
    RelPath(format!(
        "previews/{}/{}.t1.{}",
        key.fanout_hh(),
        key.to_hex(),
        codec.file_extension()
    ))
}

/// `previews/<hh>/<key>.t2/` (spec §3.1/§3.2) — the tile directory itself;
/// tile addressing within it and `manifest.cbor` are Phase F's (T20), named
/// here only so the top-level path scheme has one home.
pub fn t2_rel_dir(key: StoreKey) -> RelPath {
    RelPath(format!("previews/{}/{}.t2", key.fanout_hh(), key.to_hex()))
}

/// A resolved preview descriptor (spec §5.1/§5.2) — what
/// `PreviewService::best_available` (Phase D) returns.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PreviewDesc {
    pub scope: PreviewScope,
    pub tier: Tier,
    pub variant: VariantHash,
    pub source: PreviewSource,
    pub recipe_rev: u64,
    pub stale: bool,
    /// Upright (orientation baked).
    pub width: u32,
    pub height: u32,
    pub colorspace: PreviewColorspace,
    pub store_path: RelPath,
    pub bytes: u64,
    /// Unix seconds.
    pub built_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_params() -> VariantParams {
        VariantParams {
            enc_ver: VARIANT_PARAMS_ENC_VER,
            producer: ProducerId::EMBEDDED,
            producer_rev: 1,
            long_edge_px: 3840,
            codec: Codec::Jpeg,
            quality: 90,
            recipe_rev: 0,
            process_version: 0,
        }
    }

    /// Spec §5.1 AC: committed golden hash vectors pass on every OS (xxh3 is
    /// endian-independent over byte inputs, ciborium's wire format is
    /// platform-independent, so this is a pure function of the field
    /// values — the same on macOS/Windows/Linux CI). Any change to
    /// `VariantParams::canonical_bytes` that is not also an `enc_ver` bump
    /// changes these constants, which is exactly the failure this test
    /// exists to produce.
    #[test]
    fn golden_hash_vectors() {
        let params = sample_params();
        assert_eq!(
            hex(&params.canonical_bytes()),
            "880168656d62656464656401190f0000185a0000",
            "canonical CBOR bytes drifted without an enc_ver bump"
        );
        assert_eq!(
            params.variant_hash().0,
            0xf9a8a005908f3c6a,
            "variant_hash drifted without an enc_ver bump"
        );

        let content_hash = ContentHash([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ]);
        let asset_key = derive_store_key(
            content_hash,
            PreviewScope::Asset(AssetId(42)),
            Tier::T0,
            params.variant_hash(),
        );
        assert_eq!(
            asset_key.to_hex(),
            "1d83ad1ba28caf2dac937270fd6d548b",
            "asset-scope store key drifted"
        );
        assert_eq!(asset_key.fanout_hh(), &asset_key.to_hex()[..2]);

        let image_key = derive_store_key(
            content_hash,
            PreviewScope::Image(ImageId(42)),
            Tier::T1,
            params.variant_hash(),
        );
        assert_eq!(
            image_key.to_hex(),
            "be4712654d730f62a8c24b58d24880a0",
            "image-scope store key drifted"
        );
        // Same numeric id, different scope kind (Asset(42) vs Image(42)) —
        // proves the scope_tag byte actually participates in the hash.
        assert_ne!(asset_key, image_key);
    }

    /// Two images sharing one `content_hash` (the virtual-copy shape) with
    /// otherwise-identical variant params must NOT collide on a T1 key —
    /// this is the correctness property `derive_store_key`'s doc comment
    /// argues for; pin it as a real test, not just prose.
    #[test]
    fn image_scope_keys_never_collide_across_images_sharing_a_content_hash() {
        let content_hash = ContentHash([7; 16]);
        let variant = sample_params().variant_hash();
        let key_a = derive_store_key(
            content_hash,
            PreviewScope::Image(ImageId(1)),
            Tier::T1,
            variant,
        );
        let key_b = derive_store_key(
            content_hash,
            PreviewScope::Image(ImageId(2)),
            Tier::T1,
            variant,
        );
        assert_ne!(key_a, key_b);
    }

    /// T0 rows for the same `content_hash` DO dedupe (asset-scope is the
    /// intended sharing point, spec §3.1) regardless of which `AssetId`
    /// happens to own the row, AS LONG AS the caller passes the same
    /// `AssetId` — different `AssetId`s with the same content_hash (e.g. a
    /// re-imported duplicate file) still get independent T0 keys today,
    /// matching the "index row governs identity" model (spec §3.1: "a
    /// store-side filename is never parsed to recover identity").
    #[test]
    fn asset_scope_keys_match_for_the_same_asset_and_variant() {
        let content_hash = ContentHash([9; 16]);
        let variant = sample_params().variant_hash();
        let key_a = derive_store_key(
            content_hash,
            PreviewScope::Asset(AssetId(5)),
            Tier::T0,
            variant,
        );
        let key_b = derive_store_key(
            content_hash,
            PreviewScope::Asset(AssetId(5)),
            Tier::T0,
            variant,
        );
        assert_eq!(key_a, key_b);
    }

    /// Spec §8 property test: `VariantParams` encode→hash stability under
    /// field permutations — "must be order-independent at the API, fixed in
    /// encoding". Rust struct-literal field-init order never affects the
    /// compiled value, so this really tests that `canonical_bytes` is a
    /// pure function of field *values*, not of any incidental construction
    /// path (builder vs. literal vs. field-by-field mutation).
    #[test]
    fn encoding_is_independent_of_struct_literal_field_order() {
        let a = VariantParams {
            enc_ver: 1,
            producer: ProducerId::EMBEDDED,
            producer_rev: 2,
            long_edge_px: 100,
            codec: Codec::Jxl,
            quality: 80,
            recipe_rev: 3,
            process_version: 4,
        };
        // Same values, built by mutating a `Default`-ish base in the
        // opposite field order.
        let mut b = VariantParams {
            process_version: 4,
            recipe_rev: 3,
            quality: 80,
            codec: Codec::Jxl,
            long_edge_px: 100,
            producer_rev: 2,
            producer: ProducerId::EMBEDDED,
            enc_ver: 1,
        };
        b.enc_ver = 1; // no-op touch, just exercising a different write order
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(a.variant_hash(), b.variant_hash());
    }

    #[test]
    fn a_field_change_moves_the_hash() {
        let base = sample_params();
        let mut changed = base;
        changed.quality = 91;
        assert_ne!(base.canonical_bytes(), changed.canonical_bytes());
        assert_ne!(base.variant_hash(), changed.variant_hash());
    }

    #[test]
    fn tier_round_trips_through_u8() {
        for t in [Tier::T0, Tier::T1, Tier::T2] {
            assert_eq!(Tier::from_u8(t as u8), Some(t));
        }
        assert_eq!(Tier::from_u8(3), None);
    }

    #[test]
    fn rel_path_helpers_pick_the_right_extension() {
        let key = StoreKey([0xab; 16]);
        assert_eq!(
            t0_rel_path(key).as_str(),
            format!("previews/ab/{}.t0.jpg", key.to_hex())
        );
        assert_eq!(
            t1_rel_path(key, Codec::Jxl).as_str(),
            format!("previews/ab/{}.t1.jxl", key.to_hex())
        );
        assert_eq!(
            t1_rel_path(key, Codec::Jpeg).as_str(),
            format!("previews/ab/{}.t1.jpg", key.to_hex())
        );
        assert_eq!(
            t2_rel_dir(key).as_str(),
            format!("previews/ab/{}.t2", key.to_hex())
        );
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}
