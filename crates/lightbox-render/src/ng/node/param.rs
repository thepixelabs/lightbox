// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `ParamBlock` + typed schema + `ParamHash` (spec §3.1/§3.5).
//!
//! Owner: **A-core** (task **A3**: typed construction from recipe fragments,
//! canonical-CBOR encoding, NaN rejection, `-0.0` normalization; property test
//! that permuted field order ⇒ identical canonical bytes). `ParamHash`
//! derivation is task **B1**.
//!
//! # Canonical form
//!
//! A [`ParamBlock`]'s canonical bytes are a CBOR map whose keys are sorted
//! (fields live in a [`BTreeMap`], iterated in key order), whose `-0.0` floats
//! are normalized to `0.0`, and which can never contain `NaN` (rejected at
//! construction). The encoding is therefore **field-order independent** and
//! **stable across builds** — the property the content-keyed cache (§3.5) rides
//! on.

use std::collections::BTreeMap;

use ciborium::value::{Integer, Value as CborValue};

use crate::ng::error::NodeError;

/// A single typed parameter value (spec §3.1 — recipe fragments).
#[derive(Clone, Debug, PartialEq)]
pub enum ParamValue {
    /// A 64-bit float. `NaN` is rejected and `-0.0` normalized to `0.0`.
    Float(f64),
    /// A 64-bit signed integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// A UTF-8 string.
    Text(String),
}

/// The declared kind of a schema field (spec §3.2 `params_schema`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParamKind {
    /// A [`ParamValue::Float`].
    Float,
    /// A [`ParamValue::Int`].
    Int,
    /// A [`ParamValue::Bool`].
    Bool,
    /// A [`ParamValue::Text`].
    Text,
}

impl ParamValue {
    fn kind(&self) -> ParamKind {
        match self {
            ParamValue::Float(_) => ParamKind::Float,
            ParamValue::Int(_) => ParamKind::Int,
            ParamValue::Bool(_) => ParamKind::Bool,
            ParamValue::Text(_) => ParamKind::Text,
        }
    }
}

/// One declared parameter field: a stable name and its [`ParamKind`].
#[derive(Clone, Copy, Debug)]
pub struct FieldDecl {
    /// The field name (stable; keys the canonical map).
    pub name: &'static str,
    /// The value kind the field must carry.
    pub kind: ParamKind,
}

/// A typed parameter schema — drives [`ParamBlock`] validation and hashing
/// (spec §3.2 `params_schema`). Constructible in `const`/`static` context so a
/// node can embed a `&'static ParamsSchema` in its [`super::NodeDescriptor`].
///
/// An **empty** `fields` slice means "no declared schema" — construction then
/// accepts any well-typed fields (the M1 state, before E10/E11 populate real
/// schemas). A non-empty schema rejects unknown field names and type
/// mismatches at construction.
#[derive(Debug)]
#[non_exhaustive]
pub struct ParamsSchema {
    /// The declared fields (empty = accept-any).
    pub fields: &'static [FieldDecl],
}

impl ParamsSchema {
    /// An empty schema — accepts any well-typed fields (the scaffold default).
    pub const EMPTY: ParamsSchema = ParamsSchema { fields: &[] };

    /// A schema over a `&'static` field list.
    pub const fn new(fields: &'static [FieldDecl]) -> ParamsSchema {
        ParamsSchema { fields }
    }

    fn kind_of(&self, name: &str) -> Option<ParamKind> {
        self.fields.iter().find(|f| f.name == name).map(|f| f.kind)
    }
}

/// A `&'static` reference to a node's [`ParamsSchema`] (spec §3.2).
#[derive(Clone, Copy, Debug)]
pub struct ParamsSchemaRef(pub &'static ParamsSchema);

/// A node's validated, canonically-encoded parameter block (spec §3.1/§3.5).
///
/// Canonical CBOR: field-order independent, `-0.0` → `0.0`, `NaN` rejected at
/// construction, stable across builds.
#[derive(Clone)]
#[non_exhaustive]
pub struct ParamBlock {
    fields: BTreeMap<String, ParamValue>,
    canonical: Vec<u8>,
}

impl Default for ParamBlock {
    fn default() -> Self {
        // The empty block: a canonical CBOR empty map. Never fails.
        ParamBlock::from_fields(std::iter::empty::<(String, ParamValue)>())
            .expect("empty ParamBlock is always valid")
    }
}

impl std::fmt::Debug for ParamBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParamBlock")
            .field("fields", &self.fields)
            .field("canonical_len", &self.canonical.len())
            .finish()
    }
}

impl PartialEq for ParamBlock {
    fn eq(&self, other: &Self) -> bool {
        // Canonical bytes are the identity: equal params ⇒ equal bytes.
        self.canonical == other.canonical
    }
}
impl Eq for ParamBlock {}

impl ParamBlock {
    /// Build from an iterator of `(name, value)` fields.
    ///
    /// Validates: rejects `NaN` floats ([`NodeError::BadParams`]); rejects
    /// duplicate field names; normalizes `-0.0` → `0.0`; then encodes the
    /// canonical (sorted-key) CBOR form. Field iteration order does **not**
    /// affect the output.
    pub fn from_fields<K, I>(iter: I) -> Result<ParamBlock, NodeError>
    where
        K: Into<String>,
        I: IntoIterator<Item = (K, ParamValue)>,
    {
        let mut fields: BTreeMap<String, ParamValue> = BTreeMap::new();
        for (k, v) in iter {
            let key = k.into();
            let value = normalize(v)?;
            if fields.insert(key.clone(), value).is_some() {
                return Err(NodeError::BadParams(format!(
                    "duplicate param field {key:?}"
                )));
            }
        }
        let canonical = encode_canonical(&fields);
        Ok(ParamBlock { fields, canonical })
    }

    /// Construct from a recipe fragment (canonical or non-canonical CBOR) and
    /// validate against `schema` (spec §3.5): parses the CBOR map, type-checks
    /// against a non-empty schema, rejects `NaN`, normalizes `-0.0`, and
    /// re-encodes to the canonical form.
    pub fn from_canonical_cbor(
        bytes: &[u8],
        schema: &ParamsSchema,
    ) -> Result<ParamBlock, NodeError> {
        let value: CborValue = ciborium::de::from_reader(bytes)
            .map_err(|e| NodeError::BadParams(format!("invalid params CBOR: {e}")))?;
        let CborValue::Map(entries) = value else {
            return Err(NodeError::BadParams("params CBOR is not a map".to_owned()));
        };
        let mut fields: Vec<(String, ParamValue)> = Vec::with_capacity(entries.len());
        for (k, v) in entries {
            let CborValue::Text(name) = k else {
                return Err(NodeError::BadParams(
                    "params map has a non-text key".to_owned(),
                ));
            };
            let value = cbor_to_value(&name, v)?;
            if !schema.fields.is_empty() {
                match schema.kind_of(&name) {
                    None => {
                        return Err(NodeError::BadParams(format!(
                            "unknown param field {name:?}"
                        )))
                    }
                    Some(kind) if kind != value.kind() => {
                        return Err(NodeError::BadParams(format!(
                            "param {name:?}: expected {kind:?}, got {:?}",
                            value.kind()
                        )))
                    }
                    Some(_) => {}
                }
            }
            fields.push((name, value));
        }
        ParamBlock::from_fields(fields)
    }

    /// The canonical-CBOR bytes (field-order independent). Stable across builds.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical
    }

    /// Look up a field by name.
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.fields.get(key)
    }

    /// The `f64` value of `key`, if present and a float.
    pub fn get_f64(&self, key: &str) -> Option<f64> {
        match self.fields.get(key) {
            Some(ParamValue::Float(x)) => Some(*x),
            _ => None,
        }
    }

    /// The `f64` value of `key`, or `default` when absent / not a float.
    pub fn get_f64_or(&self, key: &str, default: f64) -> f64 {
        self.get_f64(key).unwrap_or(default)
    }

    /// The `i64` value of `key`, if present and an integer.
    pub fn get_i64(&self, key: &str) -> Option<i64> {
        match self.fields.get(key) {
            Some(ParamValue::Int(i)) => Some(*i),
            _ => None,
        }
    }

    /// The `bool` value of `key`, if present and a boolean.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.fields.get(key) {
            Some(ParamValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// The string value of `key`, if present and text.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.fields.get(key) {
            Some(ParamValue::Text(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    /// blake3 over the canonical bytes — the `param_hash` cache-key ingredient
    /// (spec §3.5). Equal params ⇒ equal hash across processes/builds, because
    /// the input is the field-order-independent canonical CBOR. B1 (Phase B) is
    /// the no-op verify of this over the compiler/executor call sites.
    pub fn hash(&self) -> ParamHash {
        ParamHash(blake3::hash(&self.canonical))
    }
}

/// Normalize a value for canonical encoding: reject `NaN`, map `-0.0` → `0.0`.
fn normalize(v: ParamValue) -> Result<ParamValue, NodeError> {
    match v {
        ParamValue::Float(x) if x.is_nan() => {
            Err(NodeError::BadParams("param float is NaN".to_owned()))
        }
        // `-0.0 == 0.0` is true, so `x + 0.0` collapses the sign bit.
        ParamValue::Float(x) => Ok(ParamValue::Float(if x == 0.0 { 0.0 } else { x })),
        other => Ok(other),
    }
}

fn value_to_cbor(v: &ParamValue) -> CborValue {
    match v {
        ParamValue::Float(x) => CborValue::Float(*x),
        ParamValue::Int(i) => CborValue::Integer(Integer::from(*i)),
        ParamValue::Bool(b) => CborValue::Bool(*b),
        ParamValue::Text(s) => CborValue::Text(s.clone()),
    }
}

fn cbor_to_value(name: &str, v: CborValue) -> Result<ParamValue, NodeError> {
    match v {
        CborValue::Float(x) => normalize(ParamValue::Float(x)),
        CborValue::Integer(i) => i64::try_from(i)
            .map(ParamValue::Int)
            .map_err(|_| NodeError::BadParams(format!("param {name:?}: integer out of i64 range"))),
        CborValue::Bool(b) => Ok(ParamValue::Bool(b)),
        CborValue::Text(s) => Ok(ParamValue::Text(s)),
        other => Err(NodeError::BadParams(format!(
            "param {name:?}: unsupported CBOR value {other:?}"
        ))),
    }
}

/// Encode a sorted field map to canonical CBOR bytes.
fn encode_canonical(fields: &BTreeMap<String, ParamValue>) -> Vec<u8> {
    let entries: Vec<(CborValue, CborValue)> = fields
        .iter()
        .map(|(k, v)| (CborValue::Text(k.clone()), value_to_cbor(v)))
        .collect();
    let map = CborValue::Map(entries);
    let mut out = Vec::new();
    // Encoding a validated (finite, well-typed) map cannot fail.
    ciborium::ser::into_writer(&map, &mut out).expect("canonical CBOR encode of validated params");
    out
}

/// blake3 over a canonical [`ParamBlock`] (spec §3.5 `param_hash`). Equal params
/// ⇒ equal hash across processes/builds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ParamHash(pub blake3::Hash);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_order_does_not_change_canonical_bytes() {
        let a = ParamBlock::from_fields([
            ("exposure", ParamValue::Float(0.5)),
            ("contrast", ParamValue::Int(20)),
            ("mono", ParamValue::Bool(true)),
        ])
        .unwrap();
        let b = ParamBlock::from_fields([
            ("mono", ParamValue::Bool(true)),
            ("exposure", ParamValue::Float(0.5)),
            ("contrast", ParamValue::Int(20)),
        ])
        .unwrap();
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(a, b);
    }

    #[test]
    fn negative_zero_normalizes() {
        let neg = ParamBlock::from_fields([("g", ParamValue::Float(-0.0))]).unwrap();
        let pos = ParamBlock::from_fields([("g", ParamValue::Float(0.0))]).unwrap();
        assert_eq!(neg.canonical_bytes(), pos.canonical_bytes());
    }

    #[test]
    fn nan_is_rejected() {
        let err = ParamBlock::from_fields([("g", ParamValue::Float(f64::NAN))]).unwrap_err();
        assert!(matches!(err, NodeError::BadParams(_)));
    }

    #[test]
    fn duplicate_field_is_rejected() {
        let err =
            ParamBlock::from_fields([("g", ParamValue::Float(1.0)), ("g", ParamValue::Float(2.0))])
                .unwrap_err();
        assert!(matches!(err, NodeError::BadParams(_)));
    }

    #[test]
    fn canonical_cbor_round_trips() {
        let a = ParamBlock::from_fields([
            ("k", ParamValue::Text("v".to_owned())),
            ("n", ParamValue::Int(-3)),
            ("x", ParamValue::Float(1.25)),
        ])
        .unwrap();
        let b = ParamBlock::from_canonical_cbor(a.canonical_bytes(), &ParamsSchema::EMPTY).unwrap();
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(b.get_f64("x"), Some(1.25));
        assert_eq!(b.get_i64("n"), Some(-3));
        assert_eq!(b.get_str("k"), Some("v"));
    }

    #[test]
    fn schema_rejects_unknown_field_and_type_mismatch() {
        static SCHEMA: ParamsSchema = ParamsSchema::new(&[FieldDecl {
            name: "exposure",
            kind: ParamKind::Float,
        }]);
        let good = ParamBlock::from_fields([("exposure", ParamValue::Float(0.3))]).unwrap();
        assert!(ParamBlock::from_canonical_cbor(good.canonical_bytes(), &SCHEMA).is_ok());

        let unknown = ParamBlock::from_fields([("bogus", ParamValue::Float(0.3))]).unwrap();
        assert!(ParamBlock::from_canonical_cbor(unknown.canonical_bytes(), &SCHEMA).is_err());

        let wrong = ParamBlock::from_fields([("exposure", ParamValue::Int(3))]).unwrap();
        assert!(ParamBlock::from_canonical_cbor(wrong.canonical_bytes(), &SCHEMA).is_err());
    }

    #[test]
    fn param_hash_is_canonical_and_order_independent() {
        // Equal params (any field order) ⇒ equal hash; distinct params differ.
        let a = ParamBlock::from_fields([
            ("exposure", ParamValue::Float(0.5)),
            ("contrast", ParamValue::Int(20)),
        ])
        .unwrap();
        let b = ParamBlock::from_fields([
            ("contrast", ParamValue::Int(20)),
            ("exposure", ParamValue::Float(0.5)),
        ])
        .unwrap();
        let c = ParamBlock::from_fields([
            ("exposure", ParamValue::Float(0.6)),
            ("contrast", ParamValue::Int(20)),
        ])
        .unwrap();
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), c.hash());
        // Hash is a pure function of the canonical bytes.
        assert_eq!(a.hash().0, blake3::hash(a.canonical_bytes()));
    }

    #[test]
    fn empty_block_is_a_cbor_empty_map() {
        // Canonical CBOR empty map is the single byte 0xA0.
        assert_eq!(ParamBlock::default().canonical_bytes(), &[0xA0]);
    }

    use proptest::prelude::*;

    fn arb_value() -> impl Strategy<Value = ParamValue> {
        prop_oneof![
            (-1_000.0f64..1_000.0).prop_map(ParamValue::Float),
            any::<i64>().prop_map(ParamValue::Int),
            any::<bool>().prop_map(ParamValue::Bool),
            "[a-z]{0,8}".prop_map(ParamValue::Text),
        ]
    }

    proptest! {
        /// A3 gate: constructing from any permutation of the same distinct-key
        /// fields yields identical canonical bytes.
        #[test]
        fn permuted_field_order_is_identical_canonical(
            map in prop::collection::hash_map("[a-z]{1,6}", arb_value(), 0..8)
        ) {
            let forward: Vec<(String, ParamValue)> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let mut reversed = forward.clone();
            reversed.reverse();

            let a = ParamBlock::from_fields(forward).unwrap();
            let b = ParamBlock::from_fields(reversed).unwrap();
            prop_assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        }
    }
}
