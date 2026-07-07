// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`XmpDoc`] — the swappable XMP packet model (spec §3.5).
//!
//! This is the **fallback substrate** (spec §1.6 / Risk R1): our own RDF/XML
//! behind the `XmpDoc` API, so a later swap to the ISO 16684 toolkit is a
//! one-crate change. **Nothing above this module may import a substrate type**
//! (the encapsulation rule, spec §3.5 / T13): callers speak only [`XmpValue`],
//! [`ArrayKind`], and the namespace-URI + local-name addressing below.
//!
//! # Coverage (fallback scope, honestly stated)
//!
//! Scalar properties and `rdf:Seq`/`rdf:Bag`/`rdf:Alt` arrays — element-form and
//! attribute-form — are modeled with full fidelity (that is every field E09 emits
//! or reads: `crs:` scalars, the tone-curve `Seq`, `lb:` provenance, and foreign
//! scalar/array passthrough such as `dc:subject`/`lr:hierarchicalSubject`).
//! Nested struct properties (`rdf:parseType="Resource"`) are **not** modeled by
//! the fallback; full struct fidelity is the ISO toolkit's job (Phase C). This is
//! recorded in `E09-deviations.md` D-1.

use std::collections::{BTreeMap, BTreeSet};

use crate::xmp::ns;

/// The three RDF array container kinds (spec §3.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArrayKind {
    /// Ordered (`rdf:Seq`) — e.g. the tone curve.
    Seq,
    /// Unordered (`rdf:Bag`) — e.g. `dc:subject`.
    Bag,
    /// Language-alternative (`rdf:Alt`) — e.g. `dc:title`.
    Alt,
}

impl ArrayKind {
    fn rdf_tag(self) -> &'static str {
        match self {
            ArrayKind::Seq => "Seq",
            ArrayKind::Bag => "Bag",
            ArrayKind::Alt => "Alt",
        }
    }

    fn from_local(local: &str) -> Option<ArrayKind> {
        match local {
            "Seq" => Some(ArrayKind::Seq),
            "Bag" => Some(ArrayKind::Bag),
            "Alt" => Some(ArrayKind::Alt),
            _ => None,
        }
    }
}

/// A typed XMP property value (spec §3.5). XMP is a stringly-typed model; [`get`]
/// always returns the stored lexical form as [`XmpValue::Text`], and the typed
/// accessors ([`as_f64`]/[`as_bool`]/[`as_str`]) parse it. Setters serialize to
/// the canonical XMP lexical form (`True`/`False` for booleans).
///
/// [`get`]: XmpDoc::get
/// [`as_f64`]: XmpValue::as_f64
/// [`as_bool`]: XmpValue::as_bool
/// [`as_str`]: XmpValue::as_str
#[derive(Clone, PartialEq, Debug)]
pub enum XmpValue {
    /// A text (or as-yet-unparsed) value.
    Text(String),
    /// A boolean (serialized `True`/`False`).
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A real number.
    Real(f64),
}

impl XmpValue {
    /// A text value.
    pub fn text<S: Into<String>>(s: S) -> XmpValue {
        XmpValue::Text(s.into())
    }

    /// The canonical XMP lexical form written into a packet.
    pub fn to_packet_string(&self) -> String {
        match self {
            XmpValue::Text(s) => s.clone(),
            XmpValue::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            XmpValue::Int(i) => i.to_string(),
            XmpValue::Real(r) => fmt_real(*r),
        }
    }

    /// The value as a string slice where it already is text, else its lexical form.
    pub fn as_str(&self) -> String {
        self.to_packet_string()
    }

    /// Parse as a real number (XMP numbers are lexical). `None` if unparseable.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            XmpValue::Real(r) => Some(*r),
            XmpValue::Int(i) => Some(*i as f64),
            XmpValue::Bool(_) => None,
            XmpValue::Text(s) => s.trim().parse::<f64>().ok(),
        }
    }

    /// Parse as a boolean (`True`/`False`, case-insensitive; also `1`/`0`).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            XmpValue::Bool(b) => Some(*b),
            XmpValue::Text(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Format a real in a stable, minimal lexical form (deterministic goldens).
fn fmt_real(r: f64) -> String {
    if !r.is_finite() {
        return "0".to_string();
    }
    // Up to 6 decimals, trailing zeros (and a bare trailing dot) trimmed.
    let mut s = format!("{r:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// Caps on untrusted packet parsing (spec §3.5 / T16). Over-limit input is a
/// typed [`XmpError`], never a panic/OOM.
#[derive(Clone, Copy, Debug)]
pub struct ParseLimits {
    /// Maximum packet size in bytes (default 16 MiB).
    pub max_bytes: usize,
    /// Maximum element nesting depth (default 64).
    pub max_depth: usize,
    /// Maximum number of properties (default 100_000).
    pub max_props: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        ParseLimits {
            max_bytes: 16 * 1024 * 1024,
            max_depth: 64,
            max_props: 100_000,
        }
    }
}

/// XMP substrate errors (spec §3.5).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum XmpError {
    /// The packet exceeded a [`ParseLimits`] bound.
    #[error("xmp packet over limit: {0}")]
    LimitExceeded(String),
    /// The packet was not well-formed / not valid UTF-8.
    #[error("xmp packet parse error: {0}")]
    Parse(String),
    /// Serialization failed (should be infallible for our own model).
    #[error("xmp serialize error: {0}")]
    Serialize(String),
    /// Sidecar I/O failed.
    #[error("xmp io error: {0}")]
    Io(String),
}

/// A parsed / built XMP packet (spec §3.5). Properties are keyed by
/// `(namespace-URI, local-name)`; the substrate stays swappable because callers
/// never see the RDF/XML underneath.
#[derive(Clone, Debug, Default)]
pub struct XmpDoc {
    props: BTreeMap<(String, String), Stored>,
    /// Learned namespace-URI → preferred-prefix (for re-emission); seeded on parse.
    learned_prefixes: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Debug)]
enum Stored {
    Simple(String),
    Array(ArrayKind, Vec<String>),
}

impl XmpDoc {
    /// An empty packet.
    pub fn new() -> XmpDoc {
        XmpDoc::default()
    }

    /// Set a scalar property (spec §3.5). Replaces any existing value.
    pub fn set(&mut self, ns: &str, path: &str, v: XmpValue) -> Result<(), XmpError> {
        self.props.insert(
            (ns.to_string(), path.to_string()),
            Stored::Simple(v.to_packet_string()),
        );
        Ok(())
    }

    /// Get a scalar property (spec §3.5). Arrays return `None` here — use
    /// [`get_array`](XmpDoc::get_array).
    pub fn get(&self, ns: &str, path: &str) -> Option<XmpValue> {
        match self.props.get(&(ns.to_string(), path.to_string())) {
            Some(Stored::Simple(s)) => Some(XmpValue::Text(s.clone())),
            _ => None,
        }
    }

    /// Remove a property (scalar or array).
    pub fn delete(&mut self, ns: &str, path: &str) {
        self.props.remove(&(ns.to_string(), path.to_string()));
    }

    /// Set an ordered/unordered/alternative array property (spec §3.5).
    pub fn set_array(
        &mut self,
        ns: &str,
        name: &str,
        kind: ArrayKind,
        items: &[XmpValue],
    ) -> Result<(), XmpError> {
        let items = items.iter().map(|v| v.to_packet_string()).collect();
        self.props.insert(
            (ns.to_string(), name.to_string()),
            Stored::Array(kind, items),
        );
        Ok(())
    }

    /// Get an array property's items (spec §3.5). `None` for a scalar / absent.
    pub fn get_array(&self, ns: &str, name: &str) -> Option<Vec<XmpValue>> {
        match self.props.get(&(ns.to_string(), name.to_string())) {
            Some(Stored::Array(_, items)) => {
                Some(items.iter().map(|s| XmpValue::Text(s.clone())).collect())
            }
            _ => None,
        }
    }

    /// The array kind of a property, if it is an array.
    pub fn array_kind(&self, ns: &str, name: &str) -> Option<ArrayKind> {
        match self.props.get(&(ns.to_string(), name.to_string())) {
            Some(Stored::Array(k, _)) => Some(*k),
            _ => None,
        }
    }

    /// True iff a property (scalar or array) is present.
    pub fn contains(&self, ns: &str, path: &str) -> bool {
        self.props.contains_key(&(ns.to_string(), path.to_string()))
    }

    /// Every property's `(namespace-URI, local-name)`, sorted. The enumeration
    /// seam the mapping layer uses to capture foreign passthrough and to
    /// partition a `crs:` import report (spec §3.6).
    pub fn property_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.props.keys().map(|(ns, p)| (ns.as_str(), p.as_str()))
    }

    /// Properties in a single namespace, by local name.
    pub fn names_in(&self, ns: &str) -> Vec<String> {
        self.props
            .keys()
            .filter(|(n, _)| n == ns)
            .map(|(_, p)| p.clone())
            .collect()
    }

    // ── serialization ────────────────────────────────────────────────────────

    /// Serialize to a canonical RDF/XML packet (spec §3.5). Deterministic:
    /// properties in sorted `(prefix, name)` order, stable prefixes, so goldens
    /// and content hashes are reproducible.
    pub fn serialize(&self) -> Result<String, XmpError> {
        let mut used: BTreeSet<&str> = BTreeSet::new();
        for (nsuri, _) in self.props.keys() {
            used.insert(nsuri.as_str());
        }
        let prefixes = self.assign_prefixes(&used);

        let mut out = String::new();
        out.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
        out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Lightbox XMP (fallback substrate)\">\n");
        out.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
        out.push_str("  <rdf:Description rdf:about=\"\"");
        for nsuri in &used {
            let p = &prefixes[*nsuri];
            out.push_str(&format!("\n   xmlns:{}=\"{}\"", p, escape_attr(nsuri)));
        }
        out.push_str(">\n");

        // Emit in (prefix, name) order for stability.
        let mut ordered: Vec<(&str, &str, &Stored)> = self
            .props
            .iter()
            .map(|((nsuri, name), v)| (prefixes[nsuri.as_str()].as_str(), name.as_str(), v))
            .collect();
        ordered.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));

        for (prefix, name, v) in ordered {
            match v {
                Stored::Simple(s) => {
                    out.push_str(&format!(
                        "   <{p}:{n}>{val}</{p}:{n}>\n",
                        p = prefix,
                        n = name,
                        val = escape_text(s)
                    ));
                }
                Stored::Array(kind, items) => {
                    let tag = kind.rdf_tag();
                    out.push_str(&format!("   <{prefix}:{name}>\n    <rdf:{tag}>\n"));
                    for it in items {
                        out.push_str(&format!("     <rdf:li>{}</rdf:li>\n", escape_text(it)));
                    }
                    out.push_str(&format!("    </rdf:{tag}>\n   </{prefix}:{name}>\n"));
                }
            }
        }

        out.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
        out.push_str("<?xpacket end=\"w\"?>");
        Ok(out)
    }

    /// The bytes of [`serialize`](XmpDoc::serialize) (UTF-8).
    pub fn to_bytes(&self) -> Result<Vec<u8>, XmpError> {
        Ok(self.serialize()?.into_bytes())
    }

    fn assign_prefixes<'a>(&'a self, used: &BTreeSet<&'a str>) -> BTreeMap<&'a str, String> {
        let mut map = BTreeMap::new();
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut counter = 0usize;
        for nsuri in used {
            let p = ns::well_known_prefix(nsuri)
                .map(str::to_string)
                .or_else(|| self.learned_prefixes.get(*nsuri).cloned())
                .filter(|p| !taken.contains(p))
                .unwrap_or_else(|| loop {
                    let cand = format!("ns{counter}");
                    counter += 1;
                    if !taken.contains(&cand) && ns::well_known_uri(&cand).is_none() {
                        break cand;
                    }
                });
            taken.insert(p.clone());
            map.insert(*nsuri, p);
        }
        map
    }

    // ── parsing ──────────────────────────────────────────────────────────────

    /// Parse an RDF/XML XMP packet under `limits` (spec §3.5). Malformed or
    /// over-limit input is a typed [`XmpError`] — never a panic.
    pub fn parse(packet: &[u8], limits: ParseLimits) -> Result<XmpDoc, XmpError> {
        if packet.len() > limits.max_bytes {
            return Err(XmpError::LimitExceeded(format!(
                "packet is {} bytes (max {})",
                packet.len(),
                limits.max_bytes
            )));
        }
        let text = std::str::from_utf8(packet)
            .map_err(|e| XmpError::Parse(format!("not valid UTF-8: {e}")))?;
        parse_impl(text, &limits)
    }
}

/// Manual namespace-tracked RDF/XML walk (see module docs for the modeled subset).
fn parse_impl(text: &str, limits: &ParseLimits) -> Result<XmpDoc, XmpError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(text);
    let cfg = reader.config_mut();
    // Do NOT trim: quick-xml delivers entity references (`&lt;` …) as separate
    // `GeneralRef` events, splitting a text run; trimming the fragments would eat
    // significant whitespace around them. We trim the *assembled* value instead.
    cfg.trim_text(false);
    cfg.expand_empty_elements = true;

    let mut doc = XmpDoc::new();
    // Scope stack of prefix→uri declarations, and a parallel identity stack.
    let mut scopes: Vec<BTreeMap<String, String>> = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();
    let mut cur: Option<PropCtx> = None;
    let mut prop_count = 0usize;

    loop {
        let ev = reader
            .read_event()
            .map_err(|e| XmpError::Parse(e.to_string()))?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                if scopes.len() >= limits.max_depth {
                    return Err(XmpError::LimitExceeded(format!(
                        "nesting exceeds depth {}",
                        limits.max_depth
                    )));
                }
                // Build this element's namespace scope from its xmlns:* attrs.
                let mut scope = BTreeMap::new();
                let raw_name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let mut attrs: Vec<(String, String)> = Vec::new();
                for a in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
                    let val = a
                        .unescape_value()
                        .map(|c| c.into_owned())
                        .unwrap_or_default();
                    if key == "xmlns" {
                        scope.insert(String::new(), val);
                    } else if let Some(pfx) = key.strip_prefix("xmlns:") {
                        scope.insert(pfx.to_string(), val);
                    } else {
                        attrs.push((key, val));
                    }
                }
                scopes.push(scope);

                let (nsuri, local) = resolve(&scopes, &raw_name);
                // Learn prefix→uri for faithful re-emission.
                if let Some((pfx, _)) = raw_name.split_once(':') {
                    if !nsuri.is_empty() {
                        doc.learned_prefixes
                            .entry(nsuri.clone())
                            .or_insert_with(|| pfx.to_string());
                    }
                }

                let parent_is_desc = names
                    .last()
                    .map(|(n, l)| n == ns::RDF && l == "Description")
                    .unwrap_or(false);

                if nsuri == ns::RDF && local == "Description" {
                    // Attribute-form properties on rdf:Description.
                    for (k, v) in &attrs {
                        let (ans, al) = resolve(&scopes, k);
                        if ans.is_empty() || ans == ns::RDF || ans == ns::XML || k == "xmlns" {
                            continue;
                        }
                        if insert_simple(&mut doc, &mut prop_count, limits, &ans, &al, v).is_err() {
                            return Err(XmpError::LimitExceeded(format!(
                                "property count exceeds {}",
                                limits.max_props
                            )));
                        }
                    }
                } else if parent_is_desc && nsuri != ns::RDF {
                    // A property element opens.
                    cur = Some(PropCtx {
                        ns: nsuri.clone(),
                        local: local.clone(),
                        text: String::new(),
                        array: None,
                        in_li: false,
                        li_text: String::new(),
                        complex: false,
                    });
                } else if nsuri == ns::RDF {
                    if let Some(kind) = ArrayKind::from_local(&local) {
                        if let Some(c) = cur.as_mut() {
                            c.array = Some((kind, Vec::new()));
                        }
                    } else if local == "li" {
                        if let Some(c) = cur.as_mut() {
                            c.in_li = true;
                            c.li_text.clear();
                        }
                    }
                } else if cur.is_some() {
                    // A non-rdf child of a property → struct field (unmodeled).
                    if let Some(c) = cur.as_mut() {
                        c.complex = true;
                    }
                }

                names.push((nsuri, local));
            }
            Event::Text(t) => {
                let s = t.xml_content().map(|c| c.into_owned()).unwrap_or_default();
                if let Some(c) = cur.as_mut() {
                    if c.in_li {
                        c.li_text.push_str(&s);
                    } else if c.array.is_none() && !c.complex {
                        c.text.push_str(&s);
                    }
                }
            }
            Event::CData(t) => {
                let s = String::from_utf8_lossy(t.as_ref()).into_owned();
                if let Some(c) = cur.as_mut() {
                    if c.in_li {
                        c.li_text.push_str(&s);
                    } else if c.array.is_none() && !c.complex {
                        c.text.push_str(&s);
                    }
                }
            }
            Event::GeneralRef(r) => {
                // Resolve an entity reference into the active text run.
                let ch = resolve_entity(&r);
                if let (Some(ch), Some(c)) = (ch, cur.as_mut()) {
                    if c.in_li {
                        c.li_text.push_str(&ch);
                    } else if c.array.is_none() && !c.complex {
                        c.text.push_str(&ch);
                    }
                }
            }
            Event::End(_) => {
                let (nsuri, local) = names.pop().unwrap_or_default();
                scopes.pop();
                if nsuri == ns::RDF && local == "li" {
                    if let Some(c) = cur.as_mut() {
                        if let Some((_, items)) = c.array.as_mut() {
                            items.push(c.li_text.trim().to_string());
                        }
                        c.li_text.clear();
                        c.in_li = false;
                    }
                } else if let Some(c) = cur.as_ref() {
                    if c.ns == nsuri && c.local == local {
                        // Closing the active property element → finalize.
                        let ctx = cur.take().expect("cur is Some");
                        if let Some((kind, items)) = ctx.array {
                            doc.props
                                .insert((ctx.ns, ctx.local), Stored::Array(kind, items));
                            prop_count += 1;
                        } else if !ctx.complex {
                            doc.props.insert(
                                (ctx.ns, ctx.local),
                                Stored::Simple(ctx.text.trim().to_string()),
                            );
                            prop_count += 1;
                        }
                        if prop_count > limits.max_props {
                            return Err(XmpError::LimitExceeded(format!(
                                "property count exceeds {}",
                                limits.max_props
                            )));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(doc)
}

struct PropCtx {
    ns: String,
    local: String,
    text: String,
    array: Option<(ArrayKind, Vec<String>)>,
    in_li: bool,
    li_text: String,
    complex: bool,
}

fn insert_simple(
    doc: &mut XmpDoc,
    count: &mut usize,
    limits: &ParseLimits,
    ns: &str,
    local: &str,
    value: &str,
) -> Result<(), ()> {
    doc.props.insert(
        (ns.to_string(), local.to_string()),
        Stored::Simple(value.to_string()),
    );
    *count += 1;
    if *count > limits.max_props {
        return Err(());
    }
    Ok(())
}

/// Resolve `prefix:local` (or an unprefixed name) against the scope stack.
fn resolve(scopes: &[BTreeMap<String, String>], raw: &str) -> (String, String) {
    let (prefix, local) = match raw.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", raw),
    };
    if prefix == "xml" {
        return (ns::XML.to_string(), local.to_string());
    }
    for scope in scopes.iter().rev() {
        if let Some(uri) = scope.get(prefix) {
            return (uri.clone(), local.to_string());
        }
    }
    (String::new(), local.to_string())
}

/// Resolve a general/character entity reference to its text (predefined XML
/// entities + numeric char refs). Unknown named entities resolve to nothing
/// (best-effort; never panics — fuzz robustness).
fn resolve_entity(r: &quick_xml::events::BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    let name = r.decode().ok()?;
    let ch = match name.as_ref() {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    };
    Some(ch.to_string())
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_round_trip() {
        let mut doc = XmpDoc::new();
        doc.set(ns::CRS, "Exposure2012", XmpValue::text("+1.50"))
            .unwrap();
        doc.set(ns::LB, "Schema", XmpValue::Int(1)).unwrap();
        doc.set(ns::CRS, "HasSettings", XmpValue::Bool(true))
            .unwrap();
        let packet = doc.serialize().unwrap();
        let back = XmpDoc::parse(packet.as_bytes(), ParseLimits::default()).unwrap();
        assert_eq!(back.get(ns::CRS, "Exposure2012").unwrap().as_str(), "+1.50");
        assert_eq!(back.get(ns::LB, "Schema").unwrap().as_str(), "1");
        assert_eq!(
            back.get(ns::CRS, "HasSettings").unwrap().as_bool(),
            Some(true)
        );
    }

    #[test]
    fn array_round_trip_seq_and_bag() {
        let mut doc = XmpDoc::new();
        doc.set_array(
            ns::CRS,
            "ToneCurvePV2012",
            ArrayKind::Seq,
            &[XmpValue::text("0, 0"), XmpValue::text("255, 255")],
        )
        .unwrap();
        doc.set_array(
            ns::DC,
            "subject",
            ArrayKind::Bag,
            &[XmpValue::text("sunset"), XmpValue::text("beach")],
        )
        .unwrap();
        let packet = doc.serialize().unwrap();
        let back = XmpDoc::parse(packet.as_bytes(), ParseLimits::default()).unwrap();
        let curve = back.get_array(ns::CRS, "ToneCurvePV2012").unwrap();
        assert_eq!(curve.len(), 2);
        assert_eq!(curve[0].as_str(), "0, 0");
        assert_eq!(
            back.array_kind(ns::CRS, "ToneCurvePV2012"),
            Some(ArrayKind::Seq)
        );
        assert_eq!(back.array_kind(ns::DC, "subject"), Some(ArrayKind::Bag));
        let subj = back.get_array(ns::DC, "subject").unwrap();
        assert_eq!(subj.len(), 2);
    }

    #[test]
    fn parses_attribute_form_properties() {
        // LR-Classic sidecars often write crs: settings as attributes.
        let packet = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:Version="15.0"
    crs:ProcessVersion="11.0"
    crs:Exposure2012="+0.75"
    crs:Contrast2012="-10">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;
        let doc = XmpDoc::parse(packet.as_bytes(), ParseLimits::default()).unwrap();
        assert_eq!(
            doc.get(ns::CRS, "Exposure2012").unwrap().as_f64(),
            Some(0.75)
        );
        assert_eq!(
            doc.get(ns::CRS, "Contrast2012").unwrap().as_f64(),
            Some(-10.0)
        );
        assert_eq!(doc.get(ns::CRS, "ProcessVersion").unwrap().as_str(), "11.0");
    }

    #[test]
    fn escaping_round_trips() {
        let mut doc = XmpDoc::new();
        doc.set(ns::DC, "rights", XmpValue::text("a < b & c > d"))
            .unwrap();
        let packet = doc.serialize().unwrap();
        let back = XmpDoc::parse(packet.as_bytes(), ParseLimits::default()).unwrap();
        assert_eq!(
            back.get(ns::DC, "rights").unwrap().as_str(),
            "a < b & c > d"
        );
    }

    #[test]
    fn over_size_limit_is_typed_error() {
        let big = vec![b' '; 32];
        let limits = ParseLimits {
            max_bytes: 8,
            ..ParseLimits::default()
        };
        assert!(matches!(
            XmpDoc::parse(&big, limits),
            Err(XmpError::LimitExceeded(_))
        ));
    }

    #[test]
    fn garbage_does_not_panic() {
        let limits = ParseLimits::default();
        for junk in [
            &b"not xml at all"[..],
            b"<x:xmpmeta><unclosed",
            b"<rdf:Description crs:X",
            &[0xff, 0xfe, 0x00, 0x01],
        ] {
            // Either Ok(empty-ish) or a typed Err; never a panic.
            let _ = XmpDoc::parse(junk, limits);
        }
    }
}
