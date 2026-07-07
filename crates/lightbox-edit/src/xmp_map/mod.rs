// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The `crs:`/`lb:` mapping layer (spec §3.6 / Phase D) — the part Lightbox owns
//! **regardless of the XMP substrate** (§1.6).
//!
//! - [`crate::Recipe::to_xmp`] — a projection (read-only materialization, §3.1): full
//!   recipe → `lb:` (schema-versioned, CBOR-faithful) **plus** best-effort `crs:`
//!   compatibility fields for Lightroom readers **plus** `xmp_passthrough` foreign
//!   fields re-emitted verbatim.
//! - [`crate::Recipe::read_xmp`] — read our own sidecar: `lb:` primary; a `crs:`-only doc
//!   falls through to [`crate::Recipe::from_lr_crs`].
//! - [`crate::Recipe::from_lr_crs`] — the `crs:` read **mechanism** (Risk 9): total, never
//!   fails on unknown fields (they land in `xmp_passthrough` + `report.skipped`),
//!   with a per-field [`CrsImportReport`]. E09 wires it into preset import + the
//!   explicit `ReadMetadata` command; the sidecar-honoring OPEN flow and legacy-PV
//!   coverage are E16 (M4).
//!
//! The normative field table backing all three is [`convert::FIELD_TABLE`],
//! mirrored in `docs/interop/crs-mapping.md` (T18).

pub mod convert;
mod from_xmp;
mod to_xmp;

use serde::Serialize;

use lightbox_meta::xmp::{sidecar, XmpError};

pub use convert::{Fidelity, FieldRow, PvClass, FIELD_TABLE};

/// `lb:` provenance / full-fidelity property names (spec §3.6).
pub mod lb {
    /// Hex of the authoritative canonical-CBOR recipe (full fidelity).
    pub const RECIPE_CBOR: &str = "RecipeCbor";
    /// `RECIPE_SCHEMA` at write time.
    pub const SCHEMA: &str = "Schema";
    /// The recipe's process version.
    pub const PROCESS_VERSION: &str = "ProcessVersion";
    /// Writing application (provenance).
    pub const CREATOR_TOOL: &str = "CreatorTool";
}

/// Provenance context for [`crate::Recipe::to_xmp`] (spec §3.6).
#[derive(Clone, Copy, Debug)]
pub struct XmpWriteCtx<'a> {
    /// App version string, emitted as `lb:CreatorTool`.
    pub app_version: &'a str,
}

impl Default for XmpWriteCtx<'_> {
    fn default() -> Self {
        XmpWriteCtx {
            app_version: concat!("Lightbox ", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// Errors from the mapping layer (spec §3.6).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum XmpMapError {
    /// The XMP substrate failed (parse/serialize/io).
    #[error("xmp substrate: {0}")]
    Substrate(#[from] XmpError),
    /// The embedded `lb:` CBOR recipe was malformed.
    #[error("lb: recipe payload malformed: {0}")]
    Malformed(String),
}

/// Which namespace a [`crate::Recipe::read_xmp`] result was reconstructed from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum XmpSource {
    /// The authoritative `lb:` full-fidelity payload (exact).
    Lb,
    /// A foreign `crs:` doc, imported via [`crate::Recipe::from_lr_crs`].
    Crs,
}

/// The result of reading a recipe out of an [`lightbox_meta::xmp::XmpDoc`] (spec §3.6).
#[derive(Clone, Debug)]
pub struct RecipeFromXmp {
    /// The reconstructed recipe.
    pub recipe: crate::Recipe,
    /// Where it came from.
    pub source: XmpSource,
    /// The fidelity report (empty/trivial on the `lb:` path; populated on `crs:`).
    pub report: CrsImportReport,
}

/// A `crs:` import + its fidelity report (spec §3.6).
#[derive(Clone, Debug)]
pub struct CrsImport {
    /// The reconstructed recipe.
    pub recipe: crate::Recipe,
    /// Per-field fidelity report.
    pub report: CrsImportReport,
}

/// One field's fidelity entry in a [`CrsImportReport`] (spec §3.6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldFidelity {
    /// Lightbox recipe field path.
    pub lightbox: String,
    /// The `crs:` property it came from.
    pub crs: String,
}

/// Per-field fidelity report for a `crs:` import (spec §3.6). The partition
/// `mapped ∪ approximate ∪ skipped` covers **every** `crs:` key seen (the E08/E16
/// expectation-setting seam).
#[derive(Clone, Debug, Default, Serialize)]
pub struct CrsImportReport {
    /// 1:1 numeric/enumerant mappings.
    pub mapped: Vec<FieldFidelity>,
    /// Domain-translated mappings (tone curve, split-toning→grade, …).
    pub approximate: Vec<FieldFidelity>,
    /// Keys skipped by design or unrecognized (e.g. `crs:MaskGroupBasedCorrections`,
    /// legacy PV≤2 tone params); preserved verbatim in `xmp_passthrough`.
    pub skipped: Vec<String>,
    /// `crs:ProcessVersion` as written by the source app.
    pub source_pv: Option<String>,
}

// ── T22 sidecar write/read helpers (the command layer rides these) ─────────────

/// Project a recipe to its sidecar `.xmp` (spec §4.2 "Project"): `to_xmp` →
/// atomic sidecar write. Targets the **sidecar** path only; the original image's
/// bytes are never touched (mandate c.2). Returns the content stamp for
/// `xmp_sync`.
///
/// The `Command::Edit(WriteMetadata)` bus arm + the debounced `Class::Background`
/// auto-write job are Phase B/E06 wiring (absent in this worktree) — see
/// `E09-deviations.md` D-2; this is the operation they invoke.
pub fn write_sidecar(
    recipe: &crate::Recipe,
    original: &std::path::Path,
    ctx: &XmpWriteCtx<'_>,
) -> Result<sidecar::SidecarStamp, XmpMapError> {
    let doc = recipe.to_xmp(ctx)?;
    let path = sidecar::sidecar_path(original);
    Ok(sidecar::write_atomic(&path, &doc)?)
}

/// Read a recipe back from an original's sidecar (spec §4.2 "Project"/read).
/// `Ok(None)` when no sidecar exists. The caller applies the result as a
/// `StepLabel::XmpRead` history step (never auto) — see `E09-deviations.md` D-2.
pub fn read_sidecar(
    original: &std::path::Path,
    probe: &lightbox_decode::AssetProbe,
) -> Result<Option<RecipeFromXmp>, XmpMapError> {
    let path = sidecar::sidecar_path(original);
    match sidecar::read(&path)? {
        Some(doc) => Ok(Some(crate::Recipe::read_xmp(&doc, probe)?)),
        None => Ok(None),
    }
}

// ── hex (lb: CBOR payload transport; no new dep) ───────────────────────────────

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    fn nib(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    let mut i = 0;
    while i < b.len() {
        out.push((nib(b[i])? << 4) | nib(b[i + 1])?);
        i += 2;
    }
    Some(out)
}

// ── foreign-field passthrough transport (§3.1.1 passthrough rule) ──────────────
//
// `Recipe.xmp_passthrough` holds foreign XMP properties verbatim so a `crs:`
// import → export round-trips them (the §6 "foreign properties survive
// read→write intact" contract). We key each field by its full
// `(namespace-URI, local-name)` — never by prefix — using the ASCII Unit
// Separator, which cannot appear in a namespace URI or an XML NCName, so the
// join is unambiguous. The value is a self-describing [`CborValue`]:
//   * a scalar is `Text(lexical)`;
//   * an array is `Array([Text(kind), Text(item0), …])` where `kind` ∈
//     `{"Seq","Bag","Alt"}`.
// A nested-struct property (`rdf:parseType="Resource"`) is not modeled by the
// fallback substrate (deviations D-4) so it never reaches this layer.

use lightbox_meta::xmp::{ArrayKind, XmpValue};

use crate::leaves::CborValue;

/// ASCII Unit Separator — the passthrough key delimiter.
pub(crate) const PT_SEP: char = '\u{1f}';

/// Build a passthrough map key from a namespace URI + local name.
pub(crate) fn pt_key(nsuri: &str, local: &str) -> String {
    format!("{nsuri}{PT_SEP}{local}")
}

/// Split a passthrough key back into `(namespace-URI, local-name)`.
pub(crate) fn pt_split(key: &str) -> Option<(&str, &str)> {
    key.split_once(PT_SEP)
}

/// The array-kind tag stored as an array value's first element.
fn array_kind_tag(kind: ArrayKind) -> &'static str {
    match kind {
        ArrayKind::Seq => "Seq",
        ArrayKind::Bag => "Bag",
        ArrayKind::Alt => "Alt",
    }
}

fn array_kind_from_tag(tag: &str) -> Option<ArrayKind> {
    match tag {
        "Seq" => Some(ArrayKind::Seq),
        "Bag" => Some(ArrayKind::Bag),
        "Alt" => Some(ArrayKind::Alt),
        _ => None,
    }
}

/// Encode a captured scalar property into its passthrough [`CborValue`].
pub(crate) fn encode_scalar(v: &XmpValue) -> CborValue {
    CborValue::Text(v.to_packet_string())
}

/// Encode a captured array property (kind + items) into its passthrough value.
pub(crate) fn encode_array(kind: ArrayKind, items: &[XmpValue]) -> CborValue {
    let mut out = Vec::with_capacity(items.len() + 1);
    out.push(CborValue::Text(array_kind_tag(kind).to_string()));
    for it in items {
        out.push(CborValue::Text(it.to_packet_string()));
    }
    CborValue::Array(out)
}

/// A decoded passthrough value ready for re-emission.
pub(crate) enum PassthroughValue {
    Scalar(String),
    Array(ArrayKind, Vec<String>),
}

/// Decode a passthrough [`CborValue`] (inverse of [`encode_scalar`]/
/// [`encode_array`]). Total: an unrecognized shape yields `None` and is skipped
/// on re-emission (defensive — only our own captures reach here).
pub(crate) fn decode_passthrough(v: &CborValue) -> Option<PassthroughValue> {
    match v {
        CborValue::Text(s) => Some(PassthroughValue::Scalar(s.clone())),
        CborValue::Array(items) => {
            let mut it = items.iter();
            let tag = match it.next() {
                Some(CborValue::Text(t)) => array_kind_from_tag(t)?,
                _ => return None,
            };
            let vals = it
                .map(|c| match c {
                    CborValue::Text(s) => s.clone(),
                    other => format!("{other:?}"),
                })
                .collect();
            Some(PassthroughValue::Array(tag, vals))
        }
        _ => None,
    }
}

/// True iff a namespace URI carries foreign metadata we round-trip verbatim
/// (i.e. it is neither ours nor an RDF/XML administrative namespace). `crs:` is
/// deliberately included: an *unmapped* `crs:` key rides passthrough too.
pub(crate) fn is_foreign_ns(nsuri: &str) -> bool {
    use lightbox_meta::xmp::ns;
    !nsuri.is_empty()
        && nsuri != ns::LB
        && nsuri != ns::RDF
        && nsuri != ns::XML
        && nsuri != "adobe:ns:meta/"
}

// ── T22 auto-write debounce policy (coalescing) ────────────────────────────────

/// Trailing-debounce coalescer for opt-in auto-write (spec T22: "debounced ≥2 s
/// per image"). Pure and clock-injected (monotonic milliseconds) so the "50-commit
/// burst coalesces to ≤2 writes" AC is unit-testable without the E06 scheduler.
///
/// A commit stream is coalesced by (a) a trailing debounce — the flush deadline
/// resets on each commit — and (b) a hard `max_wait` cap measured from the first
/// dirtying commit, so a never-quiescing stream still bounds staleness. The
/// `Class::Background` job (E06) drives `on_commit`/`due`/`flush`; the graceful
/// synchronous fallback (tests, no scheduler) is: flush eagerly on `due`.
#[derive(Clone, Copy, Debug)]
pub struct XmpWriteCoalescer {
    debounce_ms: u64,
    max_wait_ms: u64,
    first_dirty: Option<u64>,
    last_commit: Option<u64>,
    dirty: bool,
}

impl XmpWriteCoalescer {
    /// A coalescer with the given trailing-debounce and max-wait, in ms.
    pub fn new(debounce_ms: u64, max_wait_ms: u64) -> Self {
        XmpWriteCoalescer {
            debounce_ms,
            max_wait_ms: max_wait_ms.max(debounce_ms),
            first_dirty: None,
            last_commit: None,
            dirty: false,
        }
    }

    /// The spec default: ≥2 s debounce, 2 s max-wait (a burst ⇒ one write).
    pub fn default_2s() -> Self {
        XmpWriteCoalescer::new(2000, 2000)
    }

    /// Record a durable commit at monotonic time `now_ms`.
    pub fn on_commit(&mut self, now_ms: u64) {
        if !self.dirty {
            self.first_dirty = Some(now_ms);
        }
        self.dirty = true;
        self.last_commit = Some(now_ms);
    }

    /// Whether a sidecar write is due at `now_ms` (debounce elapsed since the last
    /// commit, or the max-wait cap reached).
    pub fn due(&self, now_ms: u64) -> bool {
        if !self.dirty {
            return false;
        }
        let quiescent = self
            .last_commit
            .map(|t| now_ms.saturating_sub(t) >= self.debounce_ms)
            .unwrap_or(false);
        let capped = self
            .first_dirty
            .map(|t| now_ms.saturating_sub(t) >= self.max_wait_ms)
            .unwrap_or(false);
        quiescent || capped
    }

    /// Clear the dirty state after a write at `now_ms`.
    pub fn flush(&mut self, _now_ms: u64) {
        self.dirty = false;
        self.first_dirty = None;
    }

    /// True iff there is an un-written commit pending.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 15, 16, 127, 128, 255, 0xde, 0xad, 0xbe, 0xef];
        let h = to_hex(&bytes);
        assert_eq!(from_hex(&h).unwrap(), bytes);
        assert!(from_hex("odd").is_none());
        assert!(from_hex("zz").is_none());
    }

    /// T22 AC: a 50-commit burst coalesces to ≤ 2 sidecar writes.
    #[test]
    fn burst_coalesces_to_at_most_two_writes() {
        let mut c = XmpWriteCoalescer::default_2s();
        let mut writes = 0;
        // 50 commits within a ~50 ms burst; a poller ticks every ms to 5000 ms.
        let commits: Vec<u64> = (0..50).collect();
        let mut ci = 0;
        for now in 0..=5000u64 {
            while ci < commits.len() && commits[ci] == now {
                c.on_commit(now);
                ci += 1;
            }
            if c.due(now) {
                writes += 1;
                c.flush(now);
            }
        }
        assert!(writes <= 2, "burst produced {writes} writes (want ≤ 2)");
        assert_eq!(
            writes, 1,
            "a tight burst should collapse to exactly one write"
        );
        assert!(!c.is_dirty());
    }

    /// A spread-out stream still writes, and never stalls forever. The commit
    /// stream stops at 5000; the horizon runs to 8000 so the trailing debounce
    /// (last commit + `debounce_ms`) has room to fire the final flush.
    #[test]
    fn steady_stream_bounds_staleness() {
        let mut c = XmpWriteCoalescer::new(2000, 2000);
        let mut writes = 0;
        let last_commit = 5000u64;
        for now in 0..=8000u64 {
            if now % 100 == 0 && now <= last_commit {
                c.on_commit(now);
            }
            if c.due(now) {
                writes += 1;
                c.flush(now);
            }
        }
        assert!(writes >= 1);
        assert!(
            !c.is_dirty(),
            "all commits eventually flushed by the horizon"
        );
    }
}
