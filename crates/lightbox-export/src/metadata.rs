// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Metadata emission (spec §5.6 `lightbox-meta::export`, implemented here
//! rather than in `lightbox-meta`, see "Why not `lightbox-meta`" below).
//!
//! # The privacy model: allowlist, not strip list
//!
//! An export re-encodes from pixels. [`crate::run::export_one`] hands
//! [`crate::encode`] a freshly built container and a [`MetadataBlocks`],
//! and nothing else reaches the output file. No byte of the source
//! container's metadata is copied forward at any level, so the question
//! "did we remember to remove the GPS?" never arises: the only way GPS
//! appears in an exported file is [`build`] deciding to write it, which it
//! does at [`MetadataLevel::All`] and nowhere else.
//!
//! That is why the levels in [`MetadataLevel`] are documented as an
//! allowlist. `crates/lightbox-export/tests/metadata_privacy.rs` asserts
//! both halves of the claim against files read back with an independent
//! EXIF parser: GPS present at `All` (so the assertion can detect GPS at
//! all) and absent at every other level.
//!
//! # What is written, per container
//!
//! | | JPEG | PNG | TIFF |
//! |---|---|---|---|
//! | EXIF (IFD0 + Exif IFD + GPS IFD) | APP1, `Exif\0\0` | `eXIf` chunk | see below |
//! | XMP packet | APP1, `http://ns.adobe.com/xap/1.0/\0` | `iTXt`, keyword `XML:com.adobe.xmp` | tag 700 |
//! | ICC profile | APP2 (`crate::encode`) | `iCCP` | tag 34675 |
//!
//! **TIFF does not get the packed EXIF blob.** A TIFF *is* an IFD, so the
//! fields that belong in IFD0 are written as native TIFF tags
//! ([`MetadataBlocks::tiff_tags`]: `ImageDescription`, `Make`, `Model`,
//! `Software`, `DateTime`, `Artist`, `Copyright`), and everything else the
//! policy allows, including any GPS position, rides in the XMP packet in
//! tag 700. Lightbox writes no Exif sub-IFD and no GPS sub-IFD in a TIFF.
//! The privacy claim is unaffected: at levels below
//! [`MetadataLevel::All`] neither the tags nor the packet carry camera or
//! location fields.
//!
//! **IPTC IIM is never written, in any format, at any level.** Lightbox
//! emits no `8BIM`/APP13 resource block. The IPTC-defined fields it does
//! emit (creator, copyright, description, creator contact) are XMP
//! properties in the `dc:`, `photoshop:`, `xmpRights:` and
//! `Iptc4xmpCore:` namespaces, which is IPTC's own current recommendation.
//!
//! **EXIF ASCII tags carry only the ASCII-printable subset** of a value
//! (0x20..=0x7E), because that is what the EXIF `ASCII` type is defined to
//! hold. A name with an accent in it loses the accent *in the EXIF tag*;
//! the full UTF-8 text is always in the XMP packet, which is where a
//! modern reader looks first. Every field is also capped at
//! [`MAX_FIELD_CHARS`] characters so the JPEG APP1 segments stay inside
//! their 64 KiB limit.
//!
//! # Why not `lightbox-meta`
//!
//! `lightbox-meta` is the *sidecar* XMP substrate: a flat
//! `(namespace, name) -> scalar | array` property map
//! (`crates/lightbox-meta/src/xmp/doc.rs:188`), serialized by
//! `XmpDoc::serialize` (`doc.rs:289`). Two things an export packet needs
//! are outside that model: language-alternative properties carrying
//! `xml:lang="x-default"` (`dc:rights`, `dc:description`, which `doc.rs`
//! emits as bare `<rdf:li>` without the attribute), and the
//! `Iptc4xmpCore:CreatorContactInfo` **structure**. Rather than change the
//! sidecar substrate, and with it every XMP golden in E09, this module
//! emits its own packet. The namespace URIs below are the same strings as
//! `crates/lightbox-meta/src/xmp/mod.rs:18`'s `ns` module for the
//! namespaces the two have in common, so a rename stays greppable.

use std::path::Path;

use crate::settings::{
    GpsPosition, MetadataLevel, MetadataPolicy, Rational, RightsInfo, SourceMetadata,
};

/// The per-field character cap. Applied to every text value before it
/// reaches either the EXIF tags or the XMP packet, so a pathological
/// description cannot overflow JPEG's 65533-byte APP1 segment limit.
pub const MAX_FIELD_CHARS: usize = 512;

/// What Lightbox stamps as the producing application, in the EXIF
/// `Software` tag and `xmp:CreatorTool`. Written at every
/// [`MetadataLevel`], including [`MetadataLevel::CopyrightOnly`]: it
/// describes the exported file, not the photograph.
pub const CREATOR_TOOL: &str = "Lightbox";

/// The encoder-ready metadata for one exported image, as produced by
/// [`build`]. Every field is `None`/empty when the policy allowed nothing
/// into it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MetadataBlocks {
    /// A complete little-endian TIFF structure (`II*\0` header, IFD0, an
    /// Exif sub-IFD, and a GPS sub-IFD when a position is allowed),
    /// ready to be wrapped in a JPEG APP1 `Exif\0\0` segment or written
    /// verbatim into a PNG `eXIf` chunk.
    pub exif: Option<Vec<u8>>,
    /// The UTF-8 XMP packet.
    pub xmp: Option<Vec<u8>>,
    /// `(TIFF tag number, ASCII value)` pairs for the TIFF encoder, which
    /// writes IFD0 fields natively instead of embedding [`Self::exif`].
    /// See the module doc comment.
    pub tiff_tags: Vec<(u16, String)>,
}

impl MetadataBlocks {
    /// True when nothing at all would be embedded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exif.is_none() && self.xmp.is_none() && self.tiff_tags.is_empty()
    }
}

// ─── the allowlist ─────────────────────────────────────────────────────────

/// The fields that survived the policy. Everything downstream reads this,
/// never [`SourceMetadata`] directly, so a level can only leak a field by
/// [`resolve`] putting it here.
#[derive(Clone, PartialEq, Debug, Default)]
struct Resolved {
    copyright: Option<String>,
    creator: Option<String>,
    contact_email: Option<String>,
    contact_url: Option<String>,
    description: Option<String>,
    capture_time: Option<String>,
    make: Option<String>,
    model: Option<String>,
    lens_model: Option<String>,
    exposure_time: Option<Rational>,
    f_number: Option<Rational>,
    iso: Option<u16>,
    focal_length_mm: Option<Rational>,
    gps: Option<GpsPosition>,
    width: u32,
    height: u32,
}

fn capped(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.chars().take(MAX_FIELD_CHARS).collect())
}

fn pick(primary: Option<&String>, fallback: Option<&String>) -> Option<String> {
    primary
        .and_then(|s| capped(s))
        .or_else(|| fallback.and_then(|s| capped(s)))
}

/// Applies `level` to `rights` + `source`, producing the allowlisted field
/// set. Pure and total: no IO, no failure mode.
fn resolve(
    level: MetadataLevel,
    rights: &RightsInfo,
    source: Option<&SourceMetadata>,
    width: u32,
    height: u32,
) -> Resolved {
    let src = source; // `Option<&_>` is Copy; named for readability below
    let mut out = Resolved {
        width,
        height,
        ..Resolved::default()
    };

    // Copyright is the one photograph-derived field every level writes.
    out.copyright = pick(
        rights.copyright.as_ref(),
        src.and_then(|s| s.copyright.as_ref()),
    );

    if level.writes_contact() {
        out.creator = pick(
            rights.creator.as_ref(),
            src.and_then(|s| s.creator.as_ref()),
        );
        out.contact_email = rights.contact_email.as_deref().and_then(capped);
        out.contact_url = rights.contact_url.as_deref().and_then(capped);
    }

    if level.writes_descriptive() {
        out.description = src.and_then(|s| s.description.as_deref()).and_then(capped);
        out.capture_time = src.and_then(|s| s.capture_time.as_deref()).and_then(capped);
    }

    if level.writes_camera() {
        if let Some(s) = src {
            out.make = s.make.as_deref().and_then(capped);
            out.model = s.model.as_deref().and_then(capped);
            out.lens_model = s.lens_model.as_deref().and_then(capped);
            out.exposure_time = s.exposure_time;
            out.f_number = s.f_number;
            out.iso = s.iso;
            out.focal_length_mm = s.focal_length_mm;
        }
    }

    if level.writes_location() {
        out.gps = src.and_then(|s| s.gps).filter(|g| {
            g.latitude_deg.is_finite()
                && g.longitude_deg.is_finite()
                && (-90.0..=90.0).contains(&g.latitude_deg)
                && (-180.0..=180.0).contains(&g.longitude_deg)
        });
    }

    out
}

/// Builds the metadata blocks for one exported image.
///
/// `width`/`height` are the **exported** pixel dimensions, not the
/// source's; they are the only thing here that describes the output rather
/// than the input, and they are written at every level.
#[must_use]
pub fn build(
    policy: &MetadataPolicy,
    source: Option<&SourceMetadata>,
    width: u32,
    height: u32,
) -> MetadataBlocks {
    let r = resolve(policy.level, &policy.rights, source, width, height);
    MetadataBlocks {
        exif: Some(exif_blob(&r)),
        xmp: Some(xmp_packet(&r).into_bytes()),
        tiff_tags: tiff_tags(&r),
    }
}

// ─── EXIF ──────────────────────────────────────────────────────────────────

const TY_BYTE: u16 = 1;
const TY_ASCII: u16 = 2;
const TY_SHORT: u16 = 3;
const TY_LONG: u16 = 4;
const TY_RATIONAL: u16 = 5;
const TY_UNDEFINED: u16 = 7;

// IFD0.
const TAG_IMAGE_DESCRIPTION: u16 = 0x010E;
const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_SOFTWARE: u16 = 0x0131;
const TAG_DATE_TIME: u16 = 0x0132;
const TAG_ARTIST: u16 = 0x013B;
const TAG_COPYRIGHT: u16 = 0x8298;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
// Exif sub-IFD.
const TAG_EXPOSURE_TIME: u16 = 0x829A;
const TAG_F_NUMBER: u16 = 0x829D;
const TAG_ISO: u16 = 0x8827;
const TAG_EXIF_VERSION: u16 = 0x9000;
const TAG_DATE_TIME_ORIGINAL: u16 = 0x9003;
const TAG_FOCAL_LENGTH: u16 = 0x920A;
const TAG_PIXEL_X: u16 = 0xA002;
const TAG_PIXEL_Y: u16 = 0xA003;
const TAG_LENS_MODEL: u16 = 0xA434;
// GPS sub-IFD.
const TAG_GPS_VERSION_ID: u16 = 0x0000;
const TAG_GPS_LATITUDE_REF: u16 = 0x0001;
const TAG_GPS_LATITUDE: u16 = 0x0002;
const TAG_GPS_LONGITUDE_REF: u16 = 0x0003;
const TAG_GPS_LONGITUDE: u16 = 0x0004;
const TAG_GPS_ALTITUDE_REF: u16 = 0x0005;
const TAG_GPS_ALTITUDE: u16 = 0x0006;

/// One IFD entry, with its value already serialized little-endian.
#[derive(Clone, Debug)]
struct Entry {
    tag: u16,
    ty: u16,
    count: u32,
    data: Vec<u8>,
}

/// The ASCII-printable subset of `s` (EXIF's `ASCII` type is 0x20..=0x7E
/// plus the terminating NUL; see the module doc comment on why the lossy
/// half is acceptable).
fn ascii_only(s: &str) -> String {
    s.chars()
        .filter(|c| ('\u{20}'..='\u{7e}').contains(c))
        .collect()
}

impl Entry {
    fn ascii(tag: u16, s: &str) -> Option<Entry> {
        let mut data = ascii_only(s).into_bytes();
        if data.is_empty() {
            return None;
        }
        data.push(0);
        Some(Entry {
            tag,
            ty: TY_ASCII,
            count: data.len() as u32,
            data,
        })
    }

    fn short(tag: u16, v: u16) -> Entry {
        Entry {
            tag,
            ty: TY_SHORT,
            count: 1,
            data: v.to_le_bytes().to_vec(),
        }
    }

    fn long(tag: u16, v: u32) -> Entry {
        Entry {
            tag,
            ty: TY_LONG,
            count: 1,
            data: v.to_le_bytes().to_vec(),
        }
    }

    fn rationals(tag: u16, vs: &[Rational]) -> Entry {
        let mut data = Vec::with_capacity(vs.len() * 8);
        for r in vs {
            data.extend_from_slice(&r.num.to_le_bytes());
            data.extend_from_slice(&r.den.to_le_bytes());
        }
        Entry {
            tag,
            ty: TY_RATIONAL,
            count: vs.len() as u32,
            data,
        }
    }

    fn raw(tag: u16, ty: u16, data: Vec<u8>) -> Entry {
        Entry {
            tag,
            ty,
            count: data.len() as u32,
            data,
        }
    }
}

/// Serializes one IFD (entry table, next-IFD pointer of 0, then the values
/// too large to sit inline) at absolute offset `base` from the start of
/// the TIFF header. Entries are sorted by tag, as TIFF requires. The
/// returned length does not depend on the *values* of any 4-byte inline
/// entry, which is what lets [`exif_blob`] size IFD0 before it knows where
/// the sub-IFDs will land.
fn serialize_ifd(mut entries: Vec<Entry>, base: u32) -> Vec<u8> {
    entries.sort_by_key(|e| e.tag);
    let header_len = 2 + 12 * entries.len() + 4;
    let mut ifd = Vec::with_capacity(header_len);
    let mut heap: Vec<u8> = Vec::new();

    ifd.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in &entries {
        ifd.extend_from_slice(&e.tag.to_le_bytes());
        ifd.extend_from_slice(&e.ty.to_le_bytes());
        ifd.extend_from_slice(&e.count.to_le_bytes());
        if e.data.len() <= 4 {
            let mut inline = e.data.clone();
            inline.resize(4, 0);
            ifd.extend_from_slice(&inline);
        } else {
            let offset = base + header_len as u32 + heap.len() as u32;
            ifd.extend_from_slice(&offset.to_le_bytes());
            heap.extend_from_slice(&e.data);
            if heap.len() % 2 == 1 {
                heap.push(0); // IFD values are word aligned
            }
        }
    }
    ifd.extend_from_slice(&0u32.to_le_bytes()); // no IFD1: exports carry no thumbnail
    debug_assert_eq!(ifd.len(), header_len);
    ifd.extend_from_slice(&heap);
    ifd
}

/// Degrees to EXIF's degrees/minutes/seconds rational triple.
fn dms(value: f64) -> [Rational; 3] {
    let v = value.abs();
    let deg = v.trunc();
    let minutes = (v - deg) * 60.0;
    let min = minutes.trunc();
    let sec = (minutes - min) * 60.0;
    // Floating point can land a hair over 60 seconds; clamping keeps the
    // triple canonical (the error is under 0.1 mm on the ground).
    let sec_num = ((sec * 10_000.0).round() as u32).min(599_999);
    [
        Rational::new(deg as u32, 1),
        Rational::new(min as u32, 1),
        Rational::new(sec_num, 10_000),
    ]
}

fn gps_entries(gps: GpsPosition) -> Vec<Entry> {
    let mut entries = vec![
        Entry::raw(TAG_GPS_VERSION_ID, TY_BYTE, vec![2, 3, 0, 0]),
        Entry::rationals(TAG_GPS_LATITUDE, &dms(gps.latitude_deg)),
        Entry::rationals(TAG_GPS_LONGITUDE, &dms(gps.longitude_deg)),
    ];
    if let Some(e) = Entry::ascii(
        TAG_GPS_LATITUDE_REF,
        if gps.latitude_deg < 0.0 { "S" } else { "N" },
    ) {
        entries.push(e);
    }
    if let Some(e) = Entry::ascii(
        TAG_GPS_LONGITUDE_REF,
        if gps.longitude_deg < 0.0 { "W" } else { "E" },
    ) {
        entries.push(e);
    }
    if let Some(alt) = gps.altitude_m.filter(|a| a.is_finite()) {
        entries.push(Entry::raw(
            TAG_GPS_ALTITUDE_REF,
            TY_BYTE,
            vec![u8::from(alt < 0.0)],
        ));
        entries.push(Entry::rationals(
            TAG_GPS_ALTITUDE,
            &[Rational::new((alt.abs() * 100.0).round() as u32, 100)],
        ));
    }
    entries
}

/// Builds the complete little-endian TIFF/EXIF structure for `r`.
fn exif_blob(r: &Resolved) -> Vec<u8> {
    let mut ifd0: Vec<Entry> = Vec::new();
    let mut push0 = |e: Option<Entry>| {
        if let Some(e) = e {
            ifd0.push(e);
        }
    };
    push0(Entry::ascii(TAG_SOFTWARE, CREATOR_TOOL));
    push0(
        r.copyright
            .as_deref()
            .and_then(|v| Entry::ascii(TAG_COPYRIGHT, v)),
    );
    push0(
        r.creator
            .as_deref()
            .and_then(|v| Entry::ascii(TAG_ARTIST, v)),
    );
    push0(
        r.description
            .as_deref()
            .and_then(|v| Entry::ascii(TAG_IMAGE_DESCRIPTION, v)),
    );
    push0(
        r.capture_time
            .as_deref()
            .and_then(|v| Entry::ascii(TAG_DATE_TIME, v)),
    );
    push0(r.make.as_deref().and_then(|v| Entry::ascii(TAG_MAKE, v)));
    push0(r.model.as_deref().and_then(|v| Entry::ascii(TAG_MODEL, v)));

    // The Exif sub-IFD always exists: it carries the exported dimensions,
    // which every level writes.
    let mut exif_ifd: Vec<Entry> = vec![
        Entry::raw(TAG_EXIF_VERSION, TY_UNDEFINED, b"0232".to_vec()),
        Entry::long(TAG_PIXEL_X, r.width),
        Entry::long(TAG_PIXEL_Y, r.height),
    ];
    if let Some(v) = r.capture_time.as_deref() {
        if let Some(e) = Entry::ascii(TAG_DATE_TIME_ORIGINAL, v) {
            exif_ifd.push(e);
        }
    }
    if let Some(v) = r.exposure_time {
        exif_ifd.push(Entry::rationals(TAG_EXPOSURE_TIME, &[v]));
    }
    if let Some(v) = r.f_number {
        exif_ifd.push(Entry::rationals(TAG_F_NUMBER, &[v]));
    }
    if let Some(v) = r.iso {
        exif_ifd.push(Entry::short(TAG_ISO, v));
    }
    if let Some(v) = r.focal_length_mm {
        exif_ifd.push(Entry::rationals(TAG_FOCAL_LENGTH, &[v]));
    }
    if let Some(v) = r.lens_model.as_deref() {
        if let Some(e) = Entry::ascii(TAG_LENS_MODEL, v) {
            exif_ifd.push(e);
        }
    }

    let gps_ifd = r.gps.map(gps_entries).unwrap_or_default();

    // Pointer entries first with placeholder targets, so IFD0's serialized
    // length (and hence the sub-IFD offsets) is known before the real
    // offsets are.
    ifd0.push(Entry::long(TAG_EXIF_IFD, 0));
    if !gps_ifd.is_empty() {
        ifd0.push(Entry::long(TAG_GPS_IFD, 0));
    }

    const TIFF_HEADER_LEN: u32 = 8;
    let ifd0_len = serialize_ifd(ifd0.clone(), TIFF_HEADER_LEN).len() as u32;
    let exif_off = TIFF_HEADER_LEN + ifd0_len;
    let exif_bytes = serialize_ifd(exif_ifd, exif_off);
    let gps_off = exif_off + exif_bytes.len() as u32;
    let gps_bytes = if gps_ifd.is_empty() {
        Vec::new()
    } else {
        serialize_ifd(gps_ifd, gps_off)
    };

    for e in &mut ifd0 {
        if e.tag == TAG_EXIF_IFD {
            e.data = exif_off.to_le_bytes().to_vec();
        } else if e.tag == TAG_GPS_IFD {
            e.data = gps_off.to_le_bytes().to_vec();
        }
    }
    let ifd0_bytes = serialize_ifd(ifd0, TIFF_HEADER_LEN);
    debug_assert_eq!(ifd0_bytes.len() as u32, ifd0_len);

    let mut out = Vec::with_capacity(
        TIFF_HEADER_LEN as usize + ifd0_bytes.len() + exif_bytes.len() + gps_bytes.len(),
    );
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&TIFF_HEADER_LEN.to_le_bytes());
    out.extend_from_slice(&ifd0_bytes);
    out.extend_from_slice(&exif_bytes);
    out.extend_from_slice(&gps_bytes);
    out
}

// ─── native TIFF tags ──────────────────────────────────────────────────────

fn tiff_tags(r: &Resolved) -> Vec<(u16, String)> {
    let mut out = vec![(TAG_SOFTWARE, ascii_only(CREATOR_TOOL))];
    let mut push = |tag: u16, v: Option<&String>| {
        if let Some(v) = v {
            let v = ascii_only(v);
            if !v.is_empty() {
                out.push((tag, v));
            }
        }
    };
    push(TAG_COPYRIGHT, r.copyright.as_ref());
    push(TAG_ARTIST, r.creator.as_ref());
    push(TAG_IMAGE_DESCRIPTION, r.description.as_ref());
    push(TAG_DATE_TIME, r.capture_time.as_ref());
    push(TAG_MAKE, r.make.as_ref());
    push(TAG_MODEL, r.model.as_ref());
    out.sort_by_key(|(tag, _)| *tag);
    out
}

// ─── XMP ───────────────────────────────────────────────────────────────────

const NS_DC: (&str, &str) = ("dc", "http://purl.org/dc/elements/1.1/");
const NS_XMP: (&str, &str) = ("xmp", "http://ns.adobe.com/xap/1.0/");
const NS_TIFF: (&str, &str) = ("tiff", "http://ns.adobe.com/tiff/1.0/");
const NS_EXIF: (&str, &str) = ("exif", "http://ns.adobe.com/exif/1.0/");
const NS_EXIF_AUX: (&str, &str) = ("aux", "http://ns.adobe.com/exif/1.0/aux/");
const NS_XMP_RIGHTS: (&str, &str) = ("xmpRights", "http://ns.adobe.com/xap/1.0/rights/");
const NS_IPTC_CORE: (&str, &str) = (
    "Iptc4xmpCore",
    "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
);

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids most control characters outright.
            c if (c < '\u{20}' && c != '\n' && c != '\t') || c == '\u{7f}' => {}
            c => out.push(c),
        }
    }
    out
}

/// Accumulates properties and the namespaces they actually used, so the
/// packet declares no namespace it does not reference.
#[derive(Default)]
struct PacketBuilder {
    used: Vec<(&'static str, &'static str)>,
    body: String,
}

impl PacketBuilder {
    fn ns(&mut self, ns: (&'static str, &'static str)) -> &'static str {
        if !self.used.iter().any(|(p, _)| *p == ns.0) {
            self.used.push(ns);
        }
        ns.0
    }

    /// A simple scalar property.
    fn simple(&mut self, ns: (&'static str, &'static str), name: &str, value: &str) {
        let p = self.ns(ns);
        self.body.push_str(&format!(
            "   <{p}:{name}>{}</{p}:{name}>\n",
            escape_xml(value)
        ));
    }

    /// A language-alternative property with a single `x-default` item, the
    /// shape `dc:rights`/`dc:description`/`dc:title` are defined to have.
    fn lang_alt(&mut self, ns: (&'static str, &'static str), name: &str, value: &str) {
        let p = self.ns(ns);
        self.body.push_str(&format!(
            "   <{p}:{name}>\n    <rdf:Alt>\n     \
             <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n    </rdf:Alt>\n   </{p}:{name}>\n",
            escape_xml(value)
        ));
    }

    /// An ordered array with a single item (`dc:creator` is an `rdf:Seq`).
    fn seq_one(&mut self, ns: (&'static str, &'static str), name: &str, value: &str) {
        let p = self.ns(ns);
        self.body.push_str(&format!(
            "   <{p}:{name}>\n    <rdf:Seq>\n     <rdf:li>{}</rdf:li>\n    \
             </rdf:Seq>\n   </{p}:{name}>\n",
            escape_xml(value)
        ));
    }

    fn finish(self) -> String {
        let mut out = String::new();
        out.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
        out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Lightbox export\">\n");
        out.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
        out.push_str("  <rdf:Description rdf:about=\"\"");
        let mut used = self.used;
        used.sort_unstable();
        for (prefix, uri) in used {
            out.push_str(&format!("\n   xmlns:{prefix}=\"{uri}\""));
        }
        out.push_str(">\n");
        out.push_str(&self.body);
        out.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
        out.push_str("<?xpacket end=\"w\"?>");
        out
    }
}

/// `exif:GPSLatitude`/`GPSLongitude` use "degrees,decimal-minutes" plus a
/// hemisphere letter, not a decimal degree.
fn xmp_coord(value: f64, positive: char, negative: char) -> String {
    let hemisphere = if value < 0.0 { negative } else { positive };
    let v = value.abs();
    let deg = v.trunc();
    let minutes = (v - deg) * 60.0;
    format!("{},{:.6}{}", deg as u32, minutes, hemisphere)
}

fn xmp_packet(r: &Resolved) -> String {
    let mut b = PacketBuilder::default();

    // Written at every level: this describes the export, not the subject.
    b.simple(NS_XMP, "CreatorTool", CREATOR_TOOL);
    b.simple(NS_TIFF, "ImageWidth", &r.width.to_string());
    b.simple(NS_TIFF, "ImageLength", &r.height.to_string());

    if let Some(v) = &r.copyright {
        b.lang_alt(NS_DC, "rights", v);
    }
    if let Some(v) = &r.creator {
        b.seq_one(NS_DC, "creator", v);
    }
    if let Some(v) = &r.contact_url {
        b.simple(NS_XMP_RIGHTS, "WebStatement", v);
    }
    if r.contact_email.is_some() || r.contact_url.is_some() {
        let p = b.ns(NS_IPTC_CORE);
        let mut inner = String::new();
        if let Some(v) = &r.contact_email {
            inner.push_str(&format!(
                "     <{p}:CiEmailWork>{}</{p}:CiEmailWork>\n",
                escape_xml(v)
            ));
        }
        if let Some(v) = &r.contact_url {
            inner.push_str(&format!(
                "     <{p}:CiUrlWork>{}</{p}:CiUrlWork>\n",
                escape_xml(v)
            ));
        }
        b.body.push_str(&format!(
            "   <{p}:CreatorContactInfo rdf:parseType=\"Resource\">\n{inner}   \
             </{p}:CreatorContactInfo>\n"
        ));
    }

    if let Some(v) = &r.description {
        b.lang_alt(NS_DC, "description", v);
    }
    if let Some(v) = &r.capture_time {
        b.simple(NS_EXIF, "DateTimeOriginal", v);
    }

    if let Some(v) = &r.make {
        b.simple(NS_TIFF, "Make", v);
    }
    if let Some(v) = &r.model {
        b.simple(NS_TIFF, "Model", v);
    }
    if let Some(v) = &r.lens_model {
        b.simple(NS_EXIF_AUX, "Lens", v);
    }
    if let Some(v) = r.exposure_time {
        b.simple(NS_EXIF, "ExposureTime", &format!("{}/{}", v.num, v.den));
    }
    if let Some(v) = r.f_number {
        b.simple(NS_EXIF, "FNumber", &format!("{}/{}", v.num, v.den));
    }
    if let Some(v) = r.iso {
        b.simple(NS_EXIF, "ISOSpeedRatings", &v.to_string());
    }
    if let Some(v) = r.focal_length_mm {
        b.simple(NS_EXIF, "FocalLength", &format!("{}/{}", v.num, v.den));
    }

    if let Some(g) = r.gps {
        b.simple(NS_EXIF, "GPSLatitude", &xmp_coord(g.latitude_deg, 'N', 'S'));
        b.simple(
            NS_EXIF,
            "GPSLongitude",
            &xmp_coord(g.longitude_deg, 'E', 'W'),
        );
        if let Some(alt) = g.altitude_m.filter(|a| a.is_finite()) {
            b.simple(
                NS_EXIF,
                "GPSAltitude",
                &format!("{}/100", (alt.abs() * 100.0).round() as u32),
            );
            b.simple(NS_EXIF, "GPSAltitudeRef", if alt < 0.0 { "1" } else { "0" });
        }
    }

    b.finish()
}

// ─── reading a source file ─────────────────────────────────────────────────

/// Reads the fields [`SourceMetadata`] models out of `path`'s EXIF.
///
/// Total: an unreadable, EXIF-less or malformed file yields
/// [`SourceMetadata::default`] (every field `None`), never an error. A raw
/// file whose EXIF `kamadak-exif` cannot parse simply exports without
/// camera metadata, which is the same outcome as
/// [`MetadataLevel::CopyrightOnly`] and never a failed export.
///
/// `kamadak-exif` is already the workspace's EXIF reader
/// (`crates/lightbox-decode/src/probe/exif_fields.rs:10`); this is a second
/// caller of it, not a second EXIF parser.
#[must_use]
pub fn read_source(path: &Path) -> SourceMetadata {
    let Ok(file) = std::fs::File::open(path) else {
        return SourceMetadata::default();
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return SourceMetadata::default();
    };

    SourceMetadata {
        make: ascii_field(&exif, exif::Tag::Make),
        model: ascii_field(&exif, exif::Tag::Model),
        lens_model: ascii_field(&exif, exif::Tag::LensModel),
        capture_time: ascii_field(&exif, exif::Tag::DateTimeOriginal)
            .or_else(|| ascii_field(&exif, exif::Tag::DateTimeDigitized))
            .or_else(|| ascii_field(&exif, exif::Tag::DateTime)),
        exposure_time: rational_field(&exif, exif::Tag::ExposureTime),
        f_number: rational_field(&exif, exif::Tag::FNumber),
        iso: exif
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            .and_then(|v| u16::try_from(v).ok()),
        focal_length_mm: rational_field(&exif, exif::Tag::FocalLength),
        gps: read_gps(&exif),
        description: ascii_field(&exif, exif::Tag::ImageDescription),
        creator: ascii_field(&exif, exif::Tag::Artist),
        copyright: ascii_field(&exif, exif::Tag::Copyright),
    }
}

fn ascii_field(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let exif::Value::Ascii(chunks) = &field.value else {
        return None;
    };
    let first = chunks.first()?;
    let end = first.iter().position(|&b| b == 0).unwrap_or(first.len());
    let s = String::from_utf8_lossy(&first[..end]).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

fn rational_field(exif: &exif::Exif, tag: exif::Tag) -> Option<Rational> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let exif::Value::Rational(vs) = &field.value else {
        return None;
    };
    let r = vs.first()?;
    Some(Rational::new(r.num, r.denom))
}

/// A GPS coordinate is a degrees/minutes/seconds rational triple plus a
/// hemisphere letter in a separate tag; both must be present.
fn read_gps(exif: &exif::Exif) -> Option<GpsPosition> {
    let latitude = signed_coord(
        exif,
        exif::Tag::GPSLatitude,
        exif::Tag::GPSLatitudeRef,
        b'S',
    )?;
    let longitude = signed_coord(
        exif,
        exif::Tag::GPSLongitude,
        exif::Tag::GPSLongitudeRef,
        b'W',
    )?;
    let altitude = exif
        .get_field(exif::Tag::GPSAltitude, exif::In::PRIMARY)
        .and_then(|f| match &f.value {
            exif::Value::Rational(vs) => vs.first().map(exif::Rational::to_f64),
            _ => None,
        })
        .map(|a| {
            let below = exif
                .get_field(exif::Tag::GPSAltitudeRef, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
                == Some(1);
            if below {
                -a
            } else {
                a
            }
        });
    Some(GpsPosition {
        latitude_deg: latitude,
        longitude_deg: longitude,
        altitude_m: altitude,
    })
}

fn signed_coord(
    exif: &exif::Exif,
    coord: exif::Tag,
    reference: exif::Tag,
    negative: u8,
) -> Option<f64> {
    let field = exif.get_field(coord, exif::In::PRIMARY)?;
    let exif::Value::Rational(vs) = &field.value else {
        return None;
    };
    if vs.len() < 3 {
        return None;
    }
    let degrees = vs[0].to_f64() + vs[1].to_f64() / 60.0 + vs[2].to_f64() / 3600.0;
    let reference_field = exif.get_field(reference, exif::In::PRIMARY)?;
    let exif::Value::Ascii(chunks) = &reference_field.value else {
        return None;
    };
    let sign = if chunks.first().and_then(|c| c.first()) == Some(&negative) {
        -1.0
    } else {
        1.0
    };
    degrees.is_finite().then_some(sign * degrees)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_source() -> SourceMetadata {
        SourceMetadata {
            make: Some("Canon".to_owned()),
            model: Some("EOS R5".to_owned()),
            lens_model: Some("RF 35mm F1.8".to_owned()),
            capture_time: Some("2026:03:04 11:22:33".to_owned()),
            exposure_time: Some(Rational::new(1, 250)),
            f_number: Some(Rational::new(18, 10)),
            iso: Some(400),
            focal_length_mm: Some(Rational::new(35, 1)),
            gps: Some(GpsPosition {
                latitude_deg: 51.507_351,
                longitude_deg: -0.127_758,
                altitude_m: Some(11.0),
            }),
            description: Some("A quiet street".to_owned()),
            creator: Some("Source Artist".to_owned()),
            copyright: Some("(c) 2026 Source Artist".to_owned()),
        }
    }

    fn rights() -> RightsInfo {
        RightsInfo {
            creator: Some("Jane Doe".to_owned()),
            copyright: Some("(c) 2026 Jane Doe".to_owned()),
            contact_email: Some("jane@example.com".to_owned()),
            contact_url: Some("https://example.com".to_owned()),
        }
    }

    fn resolved_at(level: MetadataLevel) -> Resolved {
        resolve(level, &rights(), Some(&full_source()), 1600, 1200)
    }

    #[test]
    fn copyright_only_keeps_the_copyright_and_nothing_else_about_the_photo() {
        let r = resolved_at(MetadataLevel::CopyrightOnly);
        assert_eq!(r.copyright.as_deref(), Some("(c) 2026 Jane Doe"));
        assert_eq!(r.creator, None);
        assert_eq!(r.contact_email, None);
        assert_eq!(r.contact_url, None);
        assert_eq!(r.description, None);
        assert_eq!(r.capture_time, None);
        assert_eq!(r.make, None);
        assert_eq!(r.model, None);
        assert_eq!(r.lens_model, None);
        assert_eq!(r.exposure_time, None);
        assert_eq!(r.f_number, None);
        assert_eq!(r.iso, None);
        assert_eq!(r.focal_length_mm, None);
        assert_eq!(r.gps, None);
        assert_eq!((r.width, r.height), (1600, 1200));
    }

    #[test]
    fn copyright_and_contact_adds_only_the_person() {
        let r = resolved_at(MetadataLevel::CopyrightAndContact);
        assert_eq!(r.creator.as_deref(), Some("Jane Doe"));
        assert_eq!(r.contact_email.as_deref(), Some("jane@example.com"));
        assert_eq!(r.contact_url.as_deref(), Some("https://example.com"));
        assert_eq!(r.description, None);
        assert_eq!(r.capture_time, None);
        assert_eq!(r.make, None);
        assert_eq!(r.gps, None);
    }

    #[test]
    fn all_except_camera_and_location_keeps_the_date_and_description() {
        let r = resolved_at(MetadataLevel::AllExceptCameraAndLocation);
        assert_eq!(r.description.as_deref(), Some("A quiet street"));
        assert_eq!(r.capture_time.as_deref(), Some("2026:03:04 11:22:33"));
        assert_eq!(r.creator.as_deref(), Some("Jane Doe"));
        assert_eq!(r.make, None, "camera make is camera identification");
        assert_eq!(r.model, None);
        assert_eq!(r.lens_model, None);
        assert_eq!(r.exposure_time, None);
        assert_eq!(r.f_number, None);
        assert_eq!(r.iso, None);
        assert_eq!(r.focal_length_mm, None);
        assert_eq!(r.gps, None, "location is removed at this level");
    }

    #[test]
    fn all_keeps_everything_including_gps() {
        let r = resolved_at(MetadataLevel::All);
        assert_eq!(r.make.as_deref(), Some("Canon"));
        assert_eq!(r.model.as_deref(), Some("EOS R5"));
        assert_eq!(r.lens_model.as_deref(), Some("RF 35mm F1.8"));
        assert_eq!(r.iso, Some(400));
        let gps = r.gps.expect("GPS survives MetadataLevel::All");
        assert!((gps.latitude_deg - 51.507_351).abs() < 1e-9);
        assert!((gps.longitude_deg + 0.127_758).abs() < 1e-9);
    }

    #[test]
    fn settings_rights_win_over_the_source_files_own() {
        let r = resolved_at(MetadataLevel::All);
        assert_eq!(r.copyright.as_deref(), Some("(c) 2026 Jane Doe"));
        assert_eq!(r.creator.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn the_source_files_rights_are_the_fallback_when_settings_are_empty() {
        let r = resolve(
            MetadataLevel::CopyrightAndContact,
            &RightsInfo::default(),
            Some(&full_source()),
            10,
            10,
        );
        assert_eq!(r.copyright.as_deref(), Some("(c) 2026 Source Artist"));
        assert_eq!(r.creator.as_deref(), Some("Source Artist"));
    }

    #[test]
    fn out_of_range_or_non_finite_coordinates_are_dropped() {
        let bad = |lat: f64, lon: f64| SourceMetadata {
            gps: Some(GpsPosition {
                latitude_deg: lat,
                longitude_deg: lon,
                altitude_m: None,
            }),
            ..SourceMetadata::default()
        };
        for (lat, lon) in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (91.0, 0.0),
            (0.0, -181.0),
        ] {
            let r = resolve(
                MetadataLevel::All,
                &RightsInfo::default(),
                Some(&bad(lat, lon)),
                4,
                4,
            );
            assert_eq!(r.gps, None, "lat {lat} lon {lon} must be rejected");
        }
    }

    #[test]
    fn text_fields_are_capped_and_trimmed() {
        let long = "x".repeat(MAX_FIELD_CHARS + 100);
        let r = resolve(
            MetadataLevel::All,
            &RightsInfo {
                copyright: Some(format!("  {long}  ")),
                ..RightsInfo::default()
            },
            None,
            1,
            1,
        );
        assert_eq!(r.copyright.map(|s| s.len()), Some(MAX_FIELD_CHARS));
    }

    #[test]
    fn exif_blob_is_a_little_endian_tiff_with_a_gps_ifd_only_when_allowed() {
        let with_gps = exif_blob(&resolved_at(MetadataLevel::All));
        assert_eq!(&with_gps[0..4], b"II\x2a\x00", "little-endian TIFF header");
        assert_eq!(
            u32::from_le_bytes(with_gps[4..8].try_into().unwrap()),
            8,
            "IFD0 starts right after the header"
        );
        assert!(ifd0_has_tag(&with_gps, TAG_GPS_IFD));
        let without = exif_blob(&resolved_at(MetadataLevel::AllExceptCameraAndLocation));
        assert!(!ifd0_has_tag(&without, TAG_GPS_IFD));
        assert!(!ifd0_has_tag(&without, TAG_MAKE));
        assert!(ifd0_has_tag(&without, TAG_COPYRIGHT));
    }

    /// Walks IFD0's entry table looking for `tag`. Deliberately hand-rolled
    /// so this test does not lean on the same code it is checking; the
    /// end-to-end read-back with an independent parser lives in
    /// `tests/metadata_privacy.rs`.
    fn ifd0_has_tag(blob: &[u8], tag: u16) -> bool {
        let ifd0 = u32::from_le_bytes(blob[4..8].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(blob[ifd0..ifd0 + 2].try_into().unwrap()) as usize;
        (0..count).any(|i| {
            let at = ifd0 + 2 + i * 12;
            u16::from_le_bytes(blob[at..at + 2].try_into().unwrap()) == tag
        })
    }

    #[test]
    fn ifd_entries_are_sorted_by_tag_as_tiff_requires() {
        let blob = exif_blob(&resolved_at(MetadataLevel::All));
        let ifd0 = u32::from_le_bytes(blob[4..8].try_into().unwrap()) as usize;
        let count = u16::from_le_bytes(blob[ifd0..ifd0 + 2].try_into().unwrap()) as usize;
        let tags: Vec<u16> = (0..count)
            .map(|i| {
                let at = ifd0 + 2 + i * 12;
                u16::from_le_bytes(blob[at..at + 2].try_into().unwrap())
            })
            .collect();
        let mut sorted = tags.clone();
        sorted.sort_unstable();
        assert_eq!(tags, sorted);
    }

    #[test]
    fn dms_round_trips_a_known_coordinate() {
        let [d, m, s] = dms(51.507_351);
        assert_eq!((d.num, d.den), (51, 1));
        assert_eq!((m.num, m.den), (30, 1));
        let seconds = f64::from(s.num) / f64::from(s.den);
        let back = 51.0 + 30.0 / 60.0 + seconds / 3600.0;
        assert!((back - 51.507_351).abs() < 1e-6, "got {back}");
    }

    #[test]
    fn xmp_packet_declares_only_the_namespaces_it_uses() {
        let packet = xmp_packet(&resolve(
            MetadataLevel::CopyrightOnly,
            &rights(),
            Some(&full_source()),
            8,
            8,
        ));
        assert!(packet.contains("xmlns:dc="));
        assert!(!packet.contains("xmlns:exif="), "no exif properties here");
        assert!(
            !packet.contains("xmlns:Iptc4xmpCore="),
            "no contact block at CopyrightOnly"
        );
        assert!(packet.contains("<?xpacket end=\"w\"?>"));
    }

    #[test]
    fn xmp_packet_has_no_gps_below_the_all_level() {
        for level in [
            MetadataLevel::CopyrightOnly,
            MetadataLevel::CopyrightAndContact,
            MetadataLevel::AllExceptCameraAndLocation,
        ] {
            let packet = xmp_packet(&resolved_at(level));
            assert!(!packet.contains("GPS"), "{level:?} leaked a GPS property");
            assert!(!packet.contains("51,"), "{level:?} leaked a latitude");
        }
        let all = xmp_packet(&resolved_at(MetadataLevel::All));
        assert!(
            all.contains("<exif:GPSLatitude>51,30.441060N</exif:GPSLatitude>"),
            "{all}"
        );
        assert!(all.contains("<exif:GPSLongitude>0,7.665480W</exif:GPSLongitude>"));
    }

    #[test]
    fn xmp_escapes_markup_in_user_text() {
        let packet = xmp_packet(&resolve(
            MetadataLevel::CopyrightOnly,
            &RightsInfo {
                copyright: Some("<b>Jane</b> & \"co\"".to_owned()),
                ..RightsInfo::default()
            },
            None,
            1,
            1,
        ));
        assert!(packet.contains("&lt;b&gt;Jane&lt;/b&gt; &amp; &quot;co&quot;"));
        assert!(!packet.contains("<b>"));
    }

    #[test]
    fn non_ascii_survives_in_xmp_and_is_stripped_from_the_exif_tag() {
        let r = resolve(
            MetadataLevel::CopyrightOnly,
            &RightsInfo {
                copyright: Some("(c) Jose Muñoz".to_owned()),
                ..RightsInfo::default()
            },
            None,
            1,
            1,
        );
        assert!(xmp_packet(&r).contains("Muñoz"));
        assert_eq!(ascii_only("(c) Jose Muñoz"), "(c) Jose Muoz");
    }

    #[test]
    fn tiff_tags_track_the_level() {
        let all = tiff_tags(&resolved_at(MetadataLevel::All));
        assert!(all.iter().any(|(t, _)| *t == TAG_MAKE));
        let stripped = tiff_tags(&resolved_at(MetadataLevel::AllExceptCameraAndLocation));
        assert!(!stripped.iter().any(|(t, _)| *t == TAG_MAKE));
        assert!(stripped
            .iter()
            .any(|(t, v)| *t == TAG_SOFTWARE && v == "Lightbox"));
    }

    #[test]
    fn read_source_on_a_missing_or_junk_file_is_empty_not_an_error() {
        assert_eq!(
            read_source(Path::new("/definitely/not/here.jpg")),
            SourceMetadata::default()
        );
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.jpg");
        std::fs::write(&junk, b"not a jpeg at all").unwrap();
        assert_eq!(read_source(&junk), SourceMetadata::default());
    }
}
