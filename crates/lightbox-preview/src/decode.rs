// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Decode-for-display (E03 spec §3.5/§5.2, Phase B T08): a stored preview
//! (T0's verbatim JPEG, at M0) → `zune-jpeg` decode → optional downscale →
//! EXIF-orientation bake → upright RGBA8 [`DecodedPreview`].
//!
//! **Always bakes orientation** (spec §3.5: "bakes EXIF orientation" is
//! unconditional, unlike the E01-seeded [`crate::pipeline::decode_class`],
//! whose loupe path deliberately left pixels unrotated for the *old* E01
//! seed's display-transform node to rotate). This is a deliberate,
//! spec-mandated behavior change for the loupe path, and it happens to
//! close a documented gap: `lightbox-core`'s `ng`-engine wiring
//! (`render_source.rs`) notes that the `ng` engine's `xform.display` node
//! does *not* apply orientation (that's E11's geometry-node job) and that
//! portrait sources render sideways until E11 lands. Because
//! [`crate::EmbeddedPreviewProvider`] now sources both classes through this
//! module (T08, wired in `embedded.rs`), the loupe source handed to the
//! engine is upright *before* it ever reaches `ng`, the sideways-portrait
//! gap is closed for the embedded-preview path specifically. Recorded in
//! `E03-deviations.md`.
//!
//! Pure functions over bytes already on disk, no scheduling, no LRU (T09
//! wraps this in `embedded.rs`, spec §3.5's "RAM LRU... plus Neighbor
//! prefetch").

use std::path::Path;

use lightbox_types::Orientation;

use crate::pipeline;
use crate::pyramid::{PreviewColorspace, PreviewDesc, PreviewSource, Tier};
use crate::PreviewError;

/// Decoded, upright RGBA8 preview pixels (spec §5.2 `DecodedPreview`).
///
/// **Deviation from the spec's literal signature:** `pixels: Arc<[u8]>`
/// here, not `Arc<ImageBufU8>`, this crate has no `ImageBufU8` type (that
/// lives in `lightbox-render`, spec §5.3's `Produced::Pixels` payload, a
/// Phase C/E05 concern). `Arc<[u8]>` interleaved RGBA8 is exactly what
/// [`crate::DecodedImage`] (the frozen `PreviewProvider` surface) already
/// carries, and what `lightbox-core`'s `PreviewSourceProvider` already wraps
/// into a `PixelBuf` for the engine, no conversion cost at the call site
/// this actually has (`embedded.rs`). Recorded in `E03-deviations.md`.
#[derive(Clone)]
pub struct DecodedPreview {
    /// Interleaved RGBA8, tightly packed rows, upright (orientation baked).
    pub pixels: std::sync::Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub colorspace: PreviewColorspace,
    pub source: PreviewSource,
    pub tier: Tier,
}

impl std::fmt::Debug for DecodedPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedPreview")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("colorspace", &self.colorspace)
            .field("source", &self.source)
            .field("tier", &self.tier)
            .finish_non_exhaustive()
    }
}

/// Decodes a stored preview's bytes to upright RGBA8 (spec §3.5's core
/// transform, factored out from [`open_pixels`] so it's testable without a
/// [`crate::Store`], see the orientation golden test below).
///
/// `max_long_edge`: optional downscale target (never upscales, spec §3.5
/// "optionally downscales to the caller's target"; `None` for the loupe,
/// `Some(px)` for thumbnails, spec T08/T09).
pub(crate) fn decode_for_display(
    bytes: &[u8],
    orientation: Orientation,
    max_long_edge: Option<u32>,
) -> Result<(Vec<u8>, u32, u32), PreviewError> {
    let (px, w, h) = decode_container_to_rgba(bytes)?;
    let (px, w, h) = match max_long_edge {
        Some(edge) => pipeline::resize_to_fit(px, w, h, edge)?,
        None => (px, w, h),
    };
    Ok(pipeline::bake_orientation(&px, w, h, orientation))
}

/// Content-first container dispatch (Phase C, T10-T12's forward-compat
/// fix): mirrors `lightbox_decode::probe`'s own "magic bytes pick the
/// walker" convention rather than trusting `desc.store_path`'s extension.
/// T0 is always JPEG (spec §3.1); T1 is JPEG by default and JXL only when
/// built AND requested with the `jxl` feature (T11's R1 fallback,
/// `codec::effective_codec`), so in the default build every stored
/// preview is JPEG and this always takes the `zune-jpeg` branch; the JXL
/// branch exists so a JXL-coded T1 (feature `jxl`, once libjxl is
/// available) decodes correctly instead of silently mis-reading a JXL
/// codestream as JPEG.
fn decode_container_to_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), PreviewError> {
    if is_jxl_container(bytes) {
        return decode_jxl_to_rgba(bytes);
    }
    pipeline::decode_jpeg_rgba(bytes)
}

/// JPEG XL magic: a bare codestream (`FF 0A`) or the ISOBMFF-boxed
/// container (12-byte `ftyp`-style signature box, big-endian: size=12,
/// `"JXL "` brand, then `0D 0A 87 0A`).
fn is_jxl_container(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0x0A])
        || bytes.starts_with(&[
            0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ])
}

#[cfg(feature = "jxl")]
fn decode_jxl_to_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), PreviewError> {
    let rgb = crate::codec::resolve(crate::config::Codec::Jxl)
        .decode(bytes)
        .map_err(|e| PreviewError::Decode(e.to_string()))?;
    Ok((rgb_to_rgba(&rgb.px), rgb.width, rgb.height))
}

#[cfg(feature = "jxl")]
fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for chunk in rgb.chunks_exact(3) {
        out.extend_from_slice(chunk);
        out.push(255);
    }
    out
}

/// With the `jxl` feature off (default build), a JXL-magic byte stream can
/// only mean the store itself is corrupt/foreign (spec §3.2's reconcile
/// territory, Phase F), never a preview this build produced (T11's R1
/// fallback means every T1 this build writes is JPEG). A clear, typed
/// error beats silently mis-decoding it as JPEG.
#[cfg(not(feature = "jxl"))]
fn decode_jxl_to_rgba(_bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), PreviewError> {
    Err(PreviewError::Decode(
        "found a JXL-coded preview but this build was compiled without the `jxl` feature".into(),
    ))
}

/// Decodes the preview `desc` points at, reading its bytes from `store`
/// (spec §5.2 `PreviewService::open_pixels`, minus the decoded-LRU/touch
/// bookkeeping half, that's `embedded.rs`'s job, T09).
pub(crate) fn open_pixels(
    store: &crate::Store,
    desc: &PreviewDesc,
    orientation: Orientation,
    max_long_edge: Option<u32>,
) -> Result<DecodedPreview, PreviewError> {
    let abs = store.resolve(&desc.store_path);
    // Phase F (T19): hold the store's live-read guard across the actual file
    // read so a concurrent eviction pass never unlinks this exact file out
    // from under us (`Store::unlink_tracked` checks `is_referenced` first).
    let _read_guard = store.begin_read(&desc.store_path);
    let bytes = read_stored(&abs)?;
    let colorspace = effective_colorspace(&bytes, desc);
    let (px, w, h) = decode_for_display(&bytes, orientation, max_long_edge)?;
    Ok(DecodedPreview {
        pixels: std::sync::Arc::from(px.into_boxed_slice()),
        width: w,
        height: h,
        colorspace,
        source: desc.source,
        tier: desc.tier,
    })
}

/// The colour space of the pixels this decode is about to produce, reconciling
/// the catalog row's recorded tag against the stored container's own profile.
///
/// # Why this is not just `desc.colorspace`
///
/// The preview store is a **persistent cache that survives upgrades**. Rows
/// written before embedded profiles were resolved say `"icc"`, "tagged,
/// unnamed", for every Display-P3 photo in the cache, and a user who already
/// has a warm cache would keep seeing the wrong colours until something forced
/// a rebuild. Invalidating the whole store instead (bumping the variant
/// encoding) would re-extract and rewrite every preview on disk to fix a tag
/// that is already sitting in the file.
///
/// So: **when the stored container carries a profile of its own, it wins.** A
/// T0 file is the camera's verbatim JPEG with its `APP2` chain intact, so its
/// tag can simply be re-derived, the stale row upgrades itself, in memory, at
/// the cost of one marker walk. A T1 file was re-encoded from pixels and has no
/// profile, so nothing overrides the row and its recorded tag stands; that is
/// exactly the case the column exists for.
///
/// Note this can only ever *replace an unresolved tag with a resolved one or
/// re-confirm it*, it reads the same bytes the same way the producer did. A
/// row can never end up naming a space the file does not actually contain.
fn effective_colorspace(bytes: &[u8], desc: &PreviewDesc) -> PreviewColorspace {
    if !crate::extract::carries_icc_profile(bytes) {
        return desc.colorspace;
    }
    let resolved = crate::extract::sniff_colorspace(bytes);
    if resolved != desc.colorspace {
        tracing::debug!(
            target: "lightbox_preview",
            recorded = desc.colorspace.as_catalog_str(),
            resolved = resolved.as_catalog_str(),
            tier = ?desc.tier,
            "preview row's colorspace upgraded from the stored container's own ICC profile"
        );
    }
    resolved
}

fn read_stored(path: &Path) -> Result<Vec<u8>, PreviewError> {
    std::fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // The catalog row exists but the store file is gone, a
            // reconcile-worthy divergence (spec §3.2 `verify_store`, Phase
            // F), surfaced here as a plain IO error; Phase F's reconcile
            // sweep is the mechanism that heals it, not this call site.
            PreviewError::Io(format!(
                "stored preview missing on disk: {}",
                path.display()
            ))
        } else {
            PreviewError::from(e)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// T08 AC: an 8-orientation corpus decodes upright. No dedicated
    /// oriented-JPEG fixture pack exists in the pinned corpus (spec §8 names
    /// one as an Open Question, Q5), this exercises the exact same
    /// `bake_orientation` transform already proven correct per-orientation
    /// on synthetic pixel data (`pipeline::tests::orientation_bakes_match_exif_semantics`,
    /// all 8 cases) through the REAL decode path (`zune-jpeg` → bake) this
    /// module adds, using a real fixture JPEG and claiming each of the 8
    /// EXIF orientations for it in turn (mirrors how `embedded_provider.rs`'s
    /// existing thumb test already claims O6 via the locator for a real
    /// fixture). Recorded as a corpus-scope deviation in `E03-deviations.md`.
    #[test]
    fn decode_for_display_renders_upright_across_all_eight_orientations() {
        let bytes = std::fs::read(fixtures_dir().join("lightbox-tiny.jpg")).unwrap();
        let (_, base_w, base_h) = pipeline::decode_jpeg_rgba(&bytes).unwrap();
        assert_eq!(
            (base_w, base_h),
            (16, 16),
            "fixture is square: dims never transpose"
        );

        for o in [
            Orientation::O1,
            Orientation::O2,
            Orientation::O3,
            Orientation::O4,
            Orientation::O5,
            Orientation::O6,
            Orientation::O7,
            Orientation::O8,
        ] {
            let (px, w, h) = decode_for_display(&bytes, o, None).unwrap();
            assert_eq!((w, h), (16, 16), "{o:?}: square fixture stays 16x16");
            assert_eq!(px.len(), 16 * 16 * 4, "{o:?}: RGBA8 buffer size");
        }
    }

    /// A non-square real fixture actually transposes for the 90°-family,
    /// same as `pipeline`'s existing thumb-orientation test, but exercised
    /// end-to-end through `decode_for_display` (JPEG decode included, not
    /// just the pure `bake_orientation` transform).
    #[test]
    fn decode_for_display_transposes_dims_for_the_90_degree_family() {
        // fujifilm-x100.raf's largest embedded preview is 2176x1448
        // (landscape), see embedded_provider.rs's own fixture table.
        let probe = lightbox_decode::probe(&fixtures_dir().join("fujifilm-x100.raf")).unwrap();
        let info = crate::pipeline::select_preview(&probe, crate::PreviewClass::Loupe).unwrap();
        let jpeg = lightbox_decode::read_embedded(&fixtures_dir().join("fujifilm-x100.raf"), info)
            .unwrap();

        let (_, w, h) = decode_for_display(&jpeg, Orientation::O1, None).unwrap();
        assert_eq!((w, h), (2176, 1448));

        let (_, w, h) = decode_for_display(&jpeg, Orientation::O6, None).unwrap();
        assert_eq!((w, h), (1448, 2176), "O6 transposes landscape to portrait");
    }

    #[test]
    fn is_jxl_container_recognizes_both_jxl_signatures_and_not_jpeg() {
        assert!(is_jxl_container(&[0xFF, 0x0A, 0, 0]), "bare codestream");
        assert!(
            is_jxl_container(&[
                0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A, 0, 0
            ]),
            "ISOBMFF container"
        );
        assert!(!is_jxl_container(&[0xFF, 0xD8, 0xFF, 0xE0]), "JPEG SOI");
        assert!(!is_jxl_container(&[]));
    }

    /// Forward-compat guard (Phase C): with the `jxl` feature off (this
    /// build), a JXL-magic byte stream is a clear, typed decode error, not
    /// a silent mis-read as JPEG (T11's R1 fallback means this build never
    /// writes JXL bytes itself, so seeing one only ever means a foreign/
    /// corrupt store file).
    #[cfg(not(feature = "jxl"))]
    #[test]
    fn jxl_bytes_report_a_clear_error_when_the_feature_is_off() {
        let jxl_magic = [0xFFu8, 0x0A, 0, 0, 0, 0, 0, 0];
        let err = decode_for_display(&jxl_magic, Orientation::O1, None).unwrap_err();
        assert!(matches!(err, PreviewError::Decode(_)), "{err:?}");
    }

    /// `max_long_edge` downscales and never upscales (mirrors
    /// `pipeline::tests::resize_only_downscales`, exercised through the
    /// combined decode+resize+bake pipeline this module adds).
    #[test]
    fn decode_for_display_downscales_but_never_upscales() {
        let bytes = std::fs::read(fixtures_dir().join("lightbox-tiny.jpg")).unwrap();
        let (_, w, h) = decode_for_display(&bytes, Orientation::O1, Some(8)).unwrap();
        assert_eq!((w, h), (8, 8));
        let (_, w, h) = decode_for_display(&bytes, Orientation::O1, Some(1024)).unwrap();
        assert_eq!((w, h), (16, 16), "already smaller than the cap: untouched");
    }

    #[test]
    fn open_pixels_reports_io_error_for_a_missing_store_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::Store::open(&crate::config::PreviewStoreConfig::with_defaults(
            dir.path().to_path_buf(),
        ))
        .unwrap();
        let desc = desc_at("previews/00/does-not-exist.t0.jpg", PreviewColorspace::Srgb);
        let err = open_pixels(&store, &desc, Orientation::O1, None).unwrap_err();
        assert!(matches!(err, PreviewError::Io(_)), "{err:?}");
    }

    // ── colour space across the store seam (Display-P3 / stale rows) ─────────

    fn desc_at(rel: &str, colorspace: PreviewColorspace) -> PreviewDesc {
        PreviewDesc {
            scope: crate::pyramid::PreviewScope::Asset(lightbox_types::AssetId(1)),
            tier: Tier::T0,
            variant: crate::pyramid::VariantHash(0),
            source: PreviewSource::Embedded,
            recipe_rev: 0,
            stale: false,
            width: 8,
            height: 8,
            colorspace,
            store_path: crate::pyramid::RelPath(rel.to_owned()),
            bytes: 0,
            built_at: 0,
        }
    }

    /// A real, decodable 8×8 JPEG, optionally carrying `icc` as an `APP2
    /// ICC_PROFILE` segment spliced in right after SOI, the way a camera or
    /// phone writes a tagged capture.
    fn tagged_jpeg(icc: Option<&[u8]>) -> Vec<u8> {
        let px: Vec<u8> = (0..8 * 8)
            .flat_map(|i| [(i * 3) as u8, (i * 5) as u8, (255 - i * 2) as u8])
            .collect();
        let jpeg = crate::codec::resolve(crate::config::Codec::Jpeg)
            .encode(
                &crate::codec::RgbImage {
                    px,
                    width: 8,
                    height: 8,
                },
                90,
            )
            .expect("encode fixture jpeg");
        let Some(icc) = icc else { return jpeg };

        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.push(1); // chunk 1
        payload.push(1); // of 1
        payload.extend_from_slice(icc);
        let seg_len = u16::try_from(payload.len() + 2).expect("profile fits one APP2 segment");
        let mut out = jpeg[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend_from_slice(&seg_len.to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn icc_for(space: lightbox_color::RgbSourceSpace) -> Vec<u8> {
        space.reference_profile().to_icc_bytes().expect("serialise")
    }

    /// Writes `bytes` into a fresh store and decodes them through the real
    /// `open_pixels` path, returning the resolved colour space.
    fn decode_colorspace(bytes: &[u8], recorded: PreviewColorspace) -> PreviewColorspace {
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::Store::open(&crate::config::PreviewStoreConfig::with_defaults(
            dir.path().to_path_buf(),
        ))
        .unwrap();
        let desc = desc_at("previews/00/fixture.t0.jpg", recorded);
        crate::store::atomic_write(&store.resolve(&desc.store_path), bytes).unwrap();
        open_pixels(&store, &desc, Orientation::O1, None)
            .expect("decode fixture")
            .colorspace
    }

    /// **The gap, at the store seam.** A stored T0 that carries a Display-P3
    /// profile decodes as Display-P3, the tag survives all the way to
    /// `DecodedPreview`, which is what hands it to the render engine.
    #[test]
    fn a_display_p3_tagged_preview_decodes_as_display_p3() {
        let jpeg = tagged_jpeg(Some(&icc_for(lightbox_color::RgbSourceSpace::DisplayP3)));
        assert_eq!(
            decode_colorspace(&jpeg, PreviewColorspace::DisplayP3),
            PreviewColorspace::DisplayP3,
        );
    }

    /// **Stale-cache self-heal.** A preview row written by an older build says
    /// `"icc"`, "tagged, unnamed", for every Display-P3 photo already in the
    /// cache. Because a T0 file *is* the camera's verbatim JPEG, its profile is
    /// still sitting in the stored bytes, so the decode re-derives the real
    /// space instead of rendering the stale row's fallback. A warm cache
    /// therefore starts rendering correctly immediately, with no store rebuild
    /// and no migration.
    #[test]
    fn a_stale_unnamed_row_upgrades_from_the_stored_profile() {
        let jpeg = tagged_jpeg(Some(&icc_for(lightbox_color::RgbSourceSpace::DisplayP3)));
        assert_eq!(
            decode_colorspace(&jpeg, PreviewColorspace::TaggedIcc),
            PreviewColorspace::DisplayP3,
            "a legacy `icc` row must upgrade from the container's own profile",
        );
        // The same holds for a legacy `"srgb"` row over tagged bytes: the file
        // is the authority whenever it carries a profile.
        assert_eq!(
            decode_colorspace(&jpeg, PreviewColorspace::Srgb),
            PreviewColorspace::DisplayP3,
        );
    }

    /// The converse, and why the catalog column still matters: a T1 preview was
    /// re-encoded from pixels and carries **no** profile, so nothing in the file
    /// can override the row. Its recorded space must survive untouched, this is
    /// the case where dropping the column would silently lose the space.
    #[test]
    fn an_untagged_container_keeps_the_rows_recorded_space() {
        let jpeg = tagged_jpeg(None);
        for recorded in [
            PreviewColorspace::DisplayP3,
            PreviewColorspace::AdobeRgb,
            PreviewColorspace::Srgb,
            PreviewColorspace::TaggedIcc,
        ] {
            assert_eq!(
                decode_colorspace(&jpeg, recorded),
                recorded,
                "an untagged container must not overwrite its row's space",
            );
        }
    }

    /// A container tagged with a profile that cannot be named degrades to
    /// [`PreviewColorspace::TaggedIcc`] (⇒ rendered as sRGB) rather than
    /// failing the decode or inventing a space.
    #[test]
    fn an_unnamable_profile_decodes_as_an_unnamed_tag() {
        let jpeg = tagged_jpeg(Some(&vec![0xABu8; 512]));
        assert_eq!(
            decode_colorspace(&jpeg, PreviewColorspace::Srgb),
            PreviewColorspace::TaggedIcc,
        );
    }
}
