// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The privacy proof for `lightbox_export::settings::MetadataLevel`.
//!
//! These tests encode real JPEG/PNG/TIFF files through
//! `lightbox_export::encode` and then read the bytes **back** with
//! `kamadak-exif`, a parser that shares no code with
//! `lightbox_export::metadata`'s writer. Asserting that a setting was
//! passed through would prove nothing; asserting that an independent
//! parser cannot find a GPS tag in the file is the claim the feature
//! actually makes.
//!
//! Every "absent" assertion is paired with a **positive control** at
//! `MetadataLevel::All` on the identical source data. Without the control,
//! a test that finds no GPS is indistinguishable from a test that cannot
//! find GPS at all.

use lightbox_export::encode;
use lightbox_export::metadata;
use lightbox_export::pixel::QuantizedPixels;
use lightbox_export::settings::{
    BitDepth, FileFormat, GpsPosition, MetadataLevel, MetadataPolicy, Rational, RightsInfo,
    SourceMetadata,
};

/// A recognisable position (Trafalgar Square) and camera, so a leak is
/// visible by eye in a hex dump as well as by assertion.
const LAT: f64 = 51.507_351;
const LON: f64 = -0.127_758;

fn source() -> SourceMetadata {
    SourceMetadata {
        make: Some("Canon".to_owned()),
        model: Some("EOS R5".to_owned()),
        lens_model: Some("RF 35mm F1.8 MACRO IS STM".to_owned()),
        capture_time: Some("2026:03:04 11:22:33".to_owned()),
        exposure_time: Some(Rational::new(1, 250)),
        f_number: Some(Rational::new(18, 10)),
        iso: Some(400),
        focal_length_mm: Some(Rational::new(35, 1)),
        gps: Some(GpsPosition {
            latitude_deg: LAT,
            longitude_deg: LON,
            altitude_m: Some(11.0),
        }),
        description: Some("Trafalgar Square at dusk".to_owned()),
        creator: None,
        copyright: None,
    }
}

fn policy(level: MetadataLevel) -> MetadataPolicy {
    MetadataPolicy {
        level,
        rights: RightsInfo {
            creator: Some("Jane Doe".to_owned()),
            copyright: Some("(c) 2026 Jane Doe. All rights reserved.".to_owned()),
            contact_email: Some("jane@example.com".to_owned()),
            contact_url: Some("https://jane.example.com".to_owned()),
        },
    }
}

const W: u32 = 16;
const H: u32 = 12;

fn pixels8() -> QuantizedPixels {
    QuantizedPixels::U8(vec![128u8; (W * H * 3) as usize])
}

/// Encodes one file in `format` at `level`, carrying the full `source()`
/// metadata in.
fn encode_at(level: MetadataLevel, format: FileFormat) -> Vec<u8> {
    let blocks = metadata::build(&policy(level), Some(&source()), W, H);
    encode::encode(W, H, &pixels8(), format, &[], &blocks).expect("encode")
}

const JPEG: FileFormat = FileFormat::Jpeg { quality: 90 };
const PNG: FileFormat = FileFormat::Png {
    depth: BitDepth::Eight,
};
const TIFF: FileFormat = FileFormat::Tiff {
    depth: BitDepth::Eight,
};

/// Every tag an independent parser can find in the file. `None` when the
/// container carries no EXIF that `kamadak-exif` recognises.
fn read_back_tags(bytes: &[u8]) -> Option<Vec<exif::Tag>> {
    let mut cursor = std::io::Cursor::new(bytes);
    let parsed = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    Some(parsed.fields().map(|f| f.tag).collect::<Vec<_>>())
}

fn has_tag(bytes: &[u8], tag: exif::Tag) -> bool {
    read_back_tags(bytes).is_some_and(|tags| tags.contains(&tag))
}

/// True when the parser finds **any** field whose tag lives in the GPS
/// IFD, not just the coordinate tags: a leak through `GPSDateStamp` or
/// `GPSAltitude` would be just as real as one through `GPSLatitude`.
/// `exif::Context::Gps` is `kamadak-exif`'s own name for that IFD, so this
/// cannot miss a GPS tag Lightbox happens not to know about.
fn has_any_gps_field(bytes: &[u8]) -> bool {
    read_back_tags(bytes).is_some_and(|tags| tags.iter().any(|t| t.context() == exif::Context::Gps))
}

/// The coordinates as bytes, in every textual form Lightbox could write
/// them. A byte scan catches a leak through a channel the EXIF parser does
/// not look at, such as the XMP packet or a stray comment.
fn raw_scan_finds_location(bytes: &[u8]) -> bool {
    // Coordinates in every form Lightbox could write them, plus the tags a
    // partial leak would arrive through. Altitude and the GPS timestamps are
    // gated as one unit with the coordinates today, but a refactor that
    // split them out would leak a position's third axis or its capture
    // moment without tripping the coordinate needles, so they are named.
    let needles: [&[u8]; 11] = [
        b"51,30.441060N",
        b"0,7.665480W",
        b"GPSLatitude",
        b"GPSLongitude",
        b"51.507351",
        b"-0.127758",
        b"GPSAltitude",
        b"GPSDateStamp",
        b"GPSTimeStamp",
        b"11/1", // 11.0 m altitude as an EXIF rational
        b"1100/100",
    ];
    needles.iter().any(|n| find_bytes(bytes, n))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ─── the positive controls ─────────────────────────────────────────────────

#[test]
fn at_the_all_level_the_gps_is_genuinely_there() {
    // Without this test, every "GPS is absent" assertion below could pass
    // on a pipeline that never writes GPS in the first place.
    for (name, format) in [("jpeg", JPEG), ("png", PNG)] {
        let bytes = encode_at(MetadataLevel::All, format);
        assert!(
            has_any_gps_field(&bytes),
            "{name}: the read-back parser found no GPS IFD at MetadataLevel::All, \
             so the absence tests below prove nothing"
        );
        assert!(
            has_tag(&bytes, exif::Tag::GPSLatitude),
            "{name}: no GPSLatitude at MetadataLevel::All"
        );
        assert!(
            has_tag(&bytes, exif::Tag::GPSLongitude),
            "{name}: no GPSLongitude at MetadataLevel::All"
        );
    }
    // TIFF carries location in the XMP packet rather than a GPS sub-IFD
    // (see `lightbox_export::metadata`'s module doc comment), so the
    // control there is the byte scan.
    let tiff = encode_at(MetadataLevel::All, TIFF);
    assert!(
        raw_scan_finds_location(&tiff),
        "tiff: no location anywhere at MetadataLevel::All"
    );
}

#[test]
fn at_the_all_level_the_coordinates_read_back_correctly() {
    let bytes = encode_at(MetadataLevel::All, JPEG);
    let mut cursor = std::io::Cursor::new(&bytes);
    let parsed = exif::Reader::new()
        .read_from_container(&mut cursor)
        .expect("EXIF parses");

    let dms = |tag: exif::Tag| -> f64 {
        let field = parsed
            .get_field(tag, exif::In::PRIMARY)
            .expect("coordinate tag");
        let exif::Value::Rational(v) = &field.value else {
            panic!("{tag} is not rational");
        };
        v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0
    };
    let reference = |tag: exif::Tag| -> String {
        parsed
            .get_field(tag, exif::In::PRIMARY)
            .map(|f| f.display_value().to_string())
            .unwrap_or_default()
    };

    assert!((dms(exif::Tag::GPSLatitude) - LAT.abs()).abs() < 1e-5);
    assert!((dms(exif::Tag::GPSLongitude) - LON.abs()).abs() < 1e-5);
    assert!(reference(exif::Tag::GPSLatitudeRef).starts_with('N'));
    assert!(reference(exif::Tag::GPSLongitudeRef).starts_with('W'));
}

// ─── the actual privacy claim ──────────────────────────────────────────────

#[test]
fn no_level_below_all_writes_gps_into_any_container() {
    for level in [
        MetadataLevel::CopyrightOnly,
        MetadataLevel::CopyrightAndContact,
        MetadataLevel::AllExceptCameraAndLocation,
    ] {
        for (name, format) in [("jpeg", JPEG), ("png", PNG), ("tiff", TIFF)] {
            let bytes = encode_at(level, format);
            // "The parser found no GPS field" proves nothing if the parser
            // found no fields at all. Every level writes at least the
            // `Software` tag, so a parse that comes back empty means the
            // container is malformed and every absence below is vacuous.
            // TIFF is the exception by design: it carries no packed EXIF
            // blob, only native IFD0 tags plus the XMP packet, and
            // `kamadak-exif` reads TIFF IFD0 natively, so it parses too.
            assert!(
                read_back_tags(&bytes).is_some_and(|t| !t.is_empty()),
                "{name} at {level:?}: the read-back parser found no EXIF at all, so \
                 the GPS absence assertions below would pass on a broken file"
            );
            assert!(
                !has_any_gps_field(&bytes),
                "{name} at {level:?}: the file has a GPS IFD"
            );
            assert!(
                !has_tag(&bytes, exif::Tag::GPSLatitude),
                "{name} at {level:?}: GPSLatitude is present"
            );
            assert!(
                !has_tag(&bytes, exif::Tag::GPSLongitude),
                "{name} at {level:?}: GPSLongitude is present"
            );
            assert!(
                !raw_scan_finds_location(&bytes),
                "{name} at {level:?}: the raw bytes still contain the location"
            );
        }
    }
}

#[test]
fn camera_identification_stops_at_all_except_camera_and_location() {
    for (name, format) in [("jpeg", JPEG), ("png", PNG)] {
        let all = encode_at(MetadataLevel::All, format);
        assert!(
            has_tag(&all, exif::Tag::Make) && has_tag(&all, exif::Tag::Model),
            "{name}: no camera at MetadataLevel::All, the control is broken"
        );

        let stripped = encode_at(MetadataLevel::AllExceptCameraAndLocation, format);
        for tag in [
            exif::Tag::Make,
            exif::Tag::Model,
            exif::Tag::LensModel,
            exif::Tag::ExposureTime,
            exif::Tag::FNumber,
            exif::Tag::PhotographicSensitivity,
            exif::Tag::FocalLength,
        ] {
            assert!(
                !has_tag(&stripped, tag),
                "{name}: {tag} survived AllExceptCameraAndLocation"
            );
        }
        assert!(
            !find_bytes(&stripped, b"EOS R5") && !find_bytes(&stripped, b"Canon"),
            "{name}: the camera name is still in the bytes"
        );
    }
    // TIFF's camera fields are native IFD0 tags plus the XMP packet.
    let stripped = encode_at(MetadataLevel::AllExceptCameraAndLocation, TIFF);
    assert!(!find_bytes(&stripped, b"EOS R5"));
    assert!(!find_bytes(&stripped, b"Canon"));
    let all = encode_at(MetadataLevel::All, TIFF);
    assert!(find_bytes(&all, b"EOS R5"), "tiff control is broken");
}

#[test]
fn the_copyright_survives_every_level_in_every_container() {
    let needle = b"(c) 2026 Jane Doe. All rights reserved.";
    for level in [
        MetadataLevel::CopyrightOnly,
        MetadataLevel::CopyrightAndContact,
        MetadataLevel::AllExceptCameraAndLocation,
        MetadataLevel::All,
    ] {
        for (name, format) in [("jpeg", JPEG), ("png", PNG), ("tiff", TIFF)] {
            let bytes = encode_at(level, format);
            assert!(
                find_bytes(&bytes, needle),
                "{name} at {level:?}: the copyright was dropped"
            );
        }
    }
    // And it is a real EXIF tag, not only XMP text, in the two containers
    // that carry a packed EXIF block.
    for format in [JPEG, PNG] {
        let bytes = encode_at(MetadataLevel::CopyrightOnly, format);
        assert!(has_tag(&bytes, exif::Tag::Copyright));
    }
}

#[test]
fn contact_details_appear_only_from_the_contact_level_up() {
    let email = b"jane@example.com";
    let jpeg_copyright_only = encode_at(MetadataLevel::CopyrightOnly, JPEG);
    assert!(
        !find_bytes(&jpeg_copyright_only, email),
        "CopyrightOnly leaked the contact email"
    );
    assert!(
        !has_tag(&jpeg_copyright_only, exif::Tag::Artist),
        "CopyrightOnly must not write the Artist tag"
    );
    assert!(
        !find_bytes(&jpeg_copyright_only, b"<dc:creator>"),
        "CopyrightOnly must not write dc:creator either"
    );

    for level in [
        MetadataLevel::CopyrightAndContact,
        MetadataLevel::AllExceptCameraAndLocation,
        MetadataLevel::All,
    ] {
        let bytes = encode_at(level, JPEG);
        assert!(find_bytes(&bytes, email), "{level:?} dropped the email");
        assert!(
            has_tag(&bytes, exif::Tag::Artist),
            "{level:?} dropped the Artist tag"
        );
    }
}

#[test]
/// The description is user text. This level keeps it by design, and that
/// includes a caption that names a place, which is why the level's docs and
/// the CLI help say "strips the recorded position, keeps what you wrote"
/// rather than "location-free". This test pins that the text survives; the
/// privacy claim is about the structured GPS record, which the tests above
/// prove absent.
fn descriptive_fields_appear_only_from_the_all_except_level_up() {
    let caption = b"Trafalgar Square at dusk";
    for level in [
        MetadataLevel::CopyrightOnly,
        MetadataLevel::CopyrightAndContact,
    ] {
        let bytes = encode_at(level, JPEG);
        assert!(
            !find_bytes(&bytes, caption),
            "{level:?} leaked the description"
        );
        assert!(
            !has_tag(&bytes, exif::Tag::DateTimeOriginal),
            "{level:?} leaked the capture time"
        );
    }
    for level in [
        MetadataLevel::AllExceptCameraAndLocation,
        MetadataLevel::All,
    ] {
        let bytes = encode_at(level, JPEG);
        assert!(find_bytes(&bytes, caption), "{level:?} dropped it");
        assert!(has_tag(&bytes, exif::Tag::DateTimeOriginal));
    }
}

#[test]
fn no_orientation_tag_is_ever_written() {
    // Export pixels are already rotated by the render, so an orientation
    // tag would rotate them a second time in every viewer.
    for level in [MetadataLevel::CopyrightOnly, MetadataLevel::All] {
        for format in [JPEG, PNG] {
            let bytes = encode_at(level, format);
            assert!(
                !has_tag(&bytes, exif::Tag::Orientation),
                "{level:?} wrote an Orientation tag"
            );
        }
    }
}

#[test]
fn no_iptc_iim_block_is_ever_written() {
    // The claim in `lightbox_export::metadata`'s docs is absolute, so the
    // test is too: no APP13 Photoshop resource marker in a JPEG, and no
    // IPTC tag 33723 in a TIFF, at any level.
    for level in [
        MetadataLevel::CopyrightOnly,
        MetadataLevel::CopyrightAndContact,
        MetadataLevel::AllExceptCameraAndLocation,
        MetadataLevel::All,
    ] {
        let jpeg = encode_at(level, JPEG);
        assert!(
            !find_bytes(&jpeg, b"Photoshop 3.0\0"),
            "{level:?}: a JPEG got an APP13 IPTC block"
        );
        assert!(
            !find_bytes(&jpeg, b"8BIM"),
            "{level:?}: 8BIM resource found"
        );
    }
}

#[test]
fn the_exported_dimensions_are_written_not_the_sources() {
    let bytes = encode_at(MetadataLevel::CopyrightOnly, JPEG);
    let mut cursor = std::io::Cursor::new(&bytes);
    let parsed = exif::Reader::new()
        .read_from_container(&mut cursor)
        .unwrap();
    let dim = |tag: exif::Tag| {
        parsed
            .get_field(tag, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
    };
    assert_eq!(dim(exif::Tag::PixelXDimension), Some(W));
    assert_eq!(dim(exif::Tag::PixelYDimension), Some(H));
}

#[test]
fn the_software_tag_says_lightbox_at_every_level() {
    for level in [MetadataLevel::CopyrightOnly, MetadataLevel::All] {
        for (name, format) in [("jpeg", JPEG), ("png", PNG)] {
            let bytes = encode_at(level, format);
            assert!(
                has_tag(&bytes, exif::Tag::Software),
                "{name} at {level:?}: no Software tag"
            );
        }
        assert!(find_bytes(&encode_at(level, TIFF), b"Lightbox"));
    }
}

#[test]
fn an_item_with_no_source_metadata_still_exports_and_still_carries_rights() {
    for format in [JPEG, PNG, TIFF] {
        let blocks = metadata::build(&policy(MetadataLevel::All), None, W, H);
        let bytes = encode::encode(W, H, &pixels8(), format, &[], &blocks).expect("encode");
        assert!(find_bytes(&bytes, b"Jane Doe"));
        assert!(!raw_scan_finds_location(&bytes));
    }
}

#[test]
fn the_xmp_packet_is_present_and_well_formed_enough_to_locate() {
    for (name, format) in [("jpeg", JPEG), ("png", PNG), ("tiff", TIFF)] {
        let bytes = encode_at(MetadataLevel::All, format);
        assert!(
            find_bytes(&bytes, b"<x:xmpmeta"),
            "{name}: no XMP packet in the file"
        );
        assert!(find_bytes(&bytes, b"<?xpacket end=\"w\"?>"), "{name}");
        assert!(
            find_bytes(&bytes, b"W5M0MpCehiHzreSzNTczkc9d"),
            "{name}: the packet has no XMP magic id"
        );
    }
}

#[test]
fn the_encoded_pixels_survive_the_metadata() {
    // A metadata bug that corrupted the image would be worse than no
    // metadata, so check the picture is still decodable and unchanged.
    let bytes = encode_at(MetadataLevel::All, PNG);
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let mut reader = decoder.read_info().expect("PNG still decodes");
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (W, H));
    assert!(buf[..info.buffer_size()].iter().all(|&v| v == 128));

    let tiff_bytes = encode_at(MetadataLevel::All, TIFF);
    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(tiff_bytes)).unwrap();
    assert_eq!(decoder.dimensions().unwrap(), (W, H));
    let tiff::decoder::DecodingResult::U8(data) = decoder.read_image().unwrap() else {
        panic!("expected 8-bit TIFF");
    };
    assert!(data.iter().all(|&v| v == 128));
}

#[test]
fn read_source_recovers_what_the_writer_wrote() {
    // `metadata::read_source` is the other half of the loop: it turns a
    // photographer's original file into the `SourceMetadata` the policy
    // then filters. Round-tripping through a real JPEG exercises the
    // reader against bytes rather than against a struct it was handed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("with-metadata.jpg");
    std::fs::write(&path, encode_at(MetadataLevel::All, JPEG)).unwrap();

    let back = metadata::read_source(&path);
    let original = source();
    assert_eq!(back.make, original.make);
    assert_eq!(back.model, original.model);
    assert_eq!(back.lens_model, original.lens_model);
    assert_eq!(back.capture_time, original.capture_time);
    assert_eq!(back.exposure_time, original.exposure_time);
    assert_eq!(back.f_number, original.f_number);
    assert_eq!(back.iso, original.iso);
    assert_eq!(back.focal_length_mm, original.focal_length_mm);
    assert_eq!(back.description, original.description);
    // The rights came from the policy, not from `source()`, so they read
    // back as the policy's values.
    assert_eq!(back.creator.as_deref(), Some("Jane Doe"));
    assert!(back.copyright.unwrap().starts_with("(c) 2026 Jane Doe"));

    let gps = back.gps.expect("GPS round-trips");
    assert!(
        (gps.latitude_deg - LAT).abs() < 1e-5,
        "latitude {} vs {LAT}",
        gps.latitude_deg
    );
    assert!(
        (gps.longitude_deg - LON).abs() < 1e-5,
        "longitude {} vs {LON}",
        gps.longitude_deg
    );
    assert!((gps.altitude_m.unwrap() - 11.0).abs() < 1e-6);
}

#[test]
fn a_file_exported_at_copyright_only_reads_back_with_nothing_in_it() {
    // The shape a photographer cares about: export, then hand the exported
    // file to a reader and confirm there is nothing left to find.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("published.jpg");
    std::fs::write(&path, encode_at(MetadataLevel::CopyrightOnly, JPEG)).unwrap();

    let back = metadata::read_source(&path);
    assert_eq!(back.gps, None);
    assert_eq!(back.make, None);
    assert_eq!(back.model, None);
    assert_eq!(back.lens_model, None);
    assert_eq!(back.capture_time, None);
    assert_eq!(back.exposure_time, None);
    assert_eq!(back.f_number, None);
    assert_eq!(back.iso, None);
    assert_eq!(back.focal_length_mm, None);
    assert_eq!(back.description, None);
    assert_eq!(back.creator, None);
    assert!(back.copyright.is_some(), "but the copyright is still there");
}
