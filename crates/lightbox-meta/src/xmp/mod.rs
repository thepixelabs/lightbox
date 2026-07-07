// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The XMP substrate (spec §3.5): [`XmpDoc`] packet model, [`sidecar`] atomic
//! sidecar-only I/O, and [`sync`] divergence status.

pub mod doc;
pub mod sidecar;
pub mod sync;

pub use doc::{ArrayKind, ParseLimits, XmpDoc, XmpError, XmpValue};
pub use sidecar::{sidecar_path, SidecarStamp};
pub use sync::DivergenceStatus;

/// Well-known XMP namespaces (spec §3.5). Callers address properties by these
/// URIs, never by prefix — the substrate owns prefix assignment.
pub mod ns {
    /// Adobe Camera Raw settings (`crs:`).
    pub const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
    /// Lightbox namespace (`lb:`) — our full-fidelity emit.
    pub const LB: &str = "http://lightbox.app/ns/1.0/";
    /// XMP basic (`xmp:`).
    pub const XMP: &str = "http://ns.adobe.com/xap/1.0/";
    /// Dublin Core (`dc:`).
    pub const DC: &str = "http://purl.org/dc/elements/1.1/";
    /// Lightroom (`lr:`).
    pub const LR: &str = "http://ns.adobe.com/lightroom/1.0/";
    /// RDF syntax.
    pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    /// Reserved `xml:` namespace (never a property namespace).
    pub const XML: &str = "http://www.w3.org/XML/1998/namespace";
    /// TIFF metadata.
    pub const TIFF: &str = "http://ns.adobe.com/tiff/1.0/";
    /// EXIF metadata.
    pub const EXIF: &str = "http://ns.adobe.com/exif/1.0/";
    /// Camera Raw auxiliary (`aux:`).
    pub const AUX: &str = "http://ns.adobe.com/exif/1.0/aux/";
    /// Photoshop namespace.
    pub const PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";
    /// XMP media management.
    pub const XMPMM: &str = "http://ns.adobe.com/xap/1.0/mm/";

    const WELL_KNOWN: &[(&str, &str)] = &[
        (CRS, "crs"),
        (LB, "lb"),
        (XMP, "xmp"),
        (DC, "dc"),
        (LR, "lr"),
        (RDF, "rdf"),
        (TIFF, "tiff"),
        (EXIF, "exif"),
        (AUX, "aux"),
        (PHOTOSHOP, "photoshop"),
        (XMPMM, "xmpMM"),
    ];

    /// The canonical prefix for a well-known namespace URI, if any.
    pub fn well_known_prefix(uri: &str) -> Option<&'static str> {
        WELL_KNOWN.iter().find(|(u, _)| *u == uri).map(|(_, p)| *p)
    }

    /// The URI a well-known prefix maps to, if any.
    pub fn well_known_uri(prefix: &str) -> Option<&'static str> {
        WELL_KNOWN
            .iter()
            .find(|(_, p)| *p == prefix)
            .map(|(u, _)| *u)
    }
}
