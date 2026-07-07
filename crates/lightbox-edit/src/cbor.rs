// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Authoritative CBOR serialization for [`Recipe`] (spec §3.2 / T3–T4).
//!
//! The recipe is encoded as a **CBOR map keyed by text field names**, known keys
//! in fixed struct-declaration order, then a newer build's unknown top-level keys
//! appended in sorted order. This is deterministic (no `HashMap` anywhere) and an
//! **idempotent fixpoint** — `from_cbor` then `to_cbor` reproduces the exact bytes,
//! so unknown keys round-trip byte-for-byte (the §3.2 forward-compat contract) and
//! [`Recipe::canonical_hash`] is stable across the 3-OS matrix.
//!
//! Why hand-built rather than serde-derive: see deviations A-3. Generic serde on
//! `Recipe` nests `unknown` under one key and cannot capture a newer build's
//! *top-level* additions; only this codec can.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::Serialize;

use lightbox_types::ProcessVersion;

use crate::leaves::CborValue;
use crate::recipe::{Recipe, RecipeError, RecipeRead, RECIPE_SCHEMA};

fn text(s: &str) -> CborValue {
    CborValue::Text(s.to_string())
}

/// Serialize a recipe field to a canonical [`CborValue`]. Infallible for our own
/// (CBOR-representable) types.
fn to_value<T: Serialize>(v: &T) -> CborValue {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf)
        .expect("in-memory CBOR encode of a recipe field is infallible");
    ciborium::de::from_reader(&buf[..]).expect("re-decode of our own CBOR is infallible")
}

/// Decode a [`CborValue`] into a typed field.
fn from_value<T: DeserializeOwned>(v: &CborValue) -> Result<T, RecipeError> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(v, &mut buf).map_err(|e| RecipeError::Decode(e.to_string()))?;
    ciborium::de::from_reader(&buf[..]).map_err(|e| RecipeError::Decode(e.to_string()))
}

impl Recipe {
    /// Encode to the authoritative CBOR form (deterministic, unknown-key preserving).
    pub fn to_cbor(&self) -> Vec<u8> {
        let mut entries: Vec<(CborValue, CborValue)> = Vec::with_capacity(9 + self.unknown.len());
        entries.push((text("schema"), to_value(&self.schema)));
        entries.push((text("pv"), to_value(&self.pv)));
        entries.push((text("base_profile"), to_value(&self.base_profile)));
        entries.push((text("global"), to_value(&self.global)));
        entries.push((text("geometry"), to_value(&self.geometry)));
        entries.push((text("masks"), to_value(&self.masks)));
        entries.push((text("retouch"), to_value(&self.retouch)));
        entries.push((text("lb_extra"), to_value(&self.lb_extra)));
        entries.push((text("xmp_passthrough"), to_value(&self.xmp_passthrough)));
        // Unknown top-level keys (from a newer build), preserved verbatim. BTreeMap
        // iteration is sorted → deterministic; known keys never collide with these.
        for (k, v) in &self.unknown {
            entries.push((text(k), v.clone()));
        }
        let map = CborValue::Map(entries);
        let mut out = Vec::new();
        ciborium::ser::into_writer(&map, &mut out)
            .expect("in-memory CBOR encode of the recipe map is infallible");
        out
    }

    /// Decode from the authoritative CBOR form (spec §3.2). A doc whose `schema`
    /// exceeds [`RECIPE_SCHEMA`] returns [`RecipeRead::NewerSchema`] with the
    /// **original bytes preserved** — read-only, never rewritten.
    pub fn from_cbor(bytes: &[u8]) -> Result<RecipeRead, RecipeError> {
        let root: CborValue =
            ciborium::de::from_reader(bytes).map_err(|e| RecipeError::Decode(e.to_string()))?;
        let CborValue::Map(entries) = root else {
            return Err(RecipeError::Decode("recipe root is not a CBOR map".into()));
        };

        // Collect the map, rejecting non-text keys (last wins on a duplicate).
        let mut map: BTreeMap<String, CborValue> = BTreeMap::new();
        for (k, val) in entries {
            let CborValue::Text(key) = k else {
                return Err(RecipeError::Decode("recipe map has a non-text key".into()));
            };
            map.insert(key, val);
        }

        // Schema gate BEFORE decoding the body: a newer-schema doc is not our shape.
        let schema = match map.get("schema") {
            Some(v) => from_value::<u16>(v)?,
            None => return Err(RecipeError::Decode("recipe missing `schema`".into())),
        };
        if schema > RECIPE_SCHEMA {
            return Ok(RecipeRead::NewerSchema {
                raw: bytes.to_vec(),
                schema,
            });
        }

        let pv = match map.get("pv") {
            Some(v) => from_value::<ProcessVersion>(v)?,
            None => return Err(RecipeError::Decode("recipe missing `pv`".into())),
        };

        // Seed identity defaults so any known field absent from an older/newer
        // same-schema doc keeps its neutral value (forward/backward compat).
        let mut r = Recipe::identity(pv);
        r.schema = schema;
        if let Some(v) = map.remove("base_profile") {
            r.base_profile = from_value(&v)?;
        }
        if let Some(v) = map.remove("global") {
            r.global = from_value(&v)?;
        }
        if let Some(v) = map.remove("geometry") {
            r.geometry = from_value(&v)?;
        }
        if let Some(v) = map.remove("masks") {
            r.masks = from_value(&v)?;
        }
        if let Some(v) = map.remove("retouch") {
            r.retouch = from_value(&v)?;
        }
        if let Some(v) = map.remove("lb_extra") {
            r.lb_extra = from_value(&v)?;
        }
        if let Some(v) = map.remove("xmp_passthrough") {
            r.xmp_passthrough = from_value(&v)?;
        }
        for k in ["schema", "pv"] {
            map.remove(k);
        }
        // Whatever remains is a newer build's top-level keys → preserved verbatim.
        r.unknown = map;
        Ok(RecipeRead::Ok(r))
    }

    /// Deterministic content hash of the canonical CBOR encoding: xxh3-128,
    /// **big-endian** per the E01 pin convention (matches
    /// `lightbox-decode::hash_file`). Covers the whole recipe (including the
    /// extension bags). Documented as a **cache-key component for E03/E05** and
    /// the stamp source for `xmp_sync` divergence (spec §3.2 / §3.5).
    pub fn canonical_hash(&self) -> [u8; 16] {
        twox_hash::XxHash3_128::oneshot(&self.to_cbor()).to_be_bytes()
    }
}
