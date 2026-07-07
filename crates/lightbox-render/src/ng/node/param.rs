// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `ParamBlock` + typed schema + `ParamHash` (spec §3.1/§3.5).
//!
//! Owner: **A-core** (task **A3**: typed construction from recipe fragments,
//! canonical-CBOR encoding, NaN rejection, `-0.0` normalization; property test
//! that permuted field order ⇒ identical canonical bytes). `ParamHash`
//! derivation is task **B1**.

use crate::ng::error::NodeError;

/// A typed parameter schema — drives [`ParamBlock`] validation and hashing
/// (spec §3.2 `params_schema`). Constructible in `const`/`static` context so a
/// node can embed a `&'static ParamsSchema` in its [`super::NodeDescriptor`].
/// **A3 fills the fields.**
#[derive(Debug)]
#[non_exhaustive]
pub struct ParamsSchema {}

impl ParamsSchema {
    /// An empty schema — the scaffold placeholder for nodes with no params.
    pub const EMPTY: ParamsSchema = ParamsSchema {};
}

/// A `&'static` reference to a node's [`ParamsSchema`] (spec §3.2).
#[derive(Clone, Copy, Debug)]
pub struct ParamsSchemaRef(pub &'static ParamsSchema);

/// A node's validated, canonically-encoded parameter block (spec §3.1/§3.5).
///
/// Canonical CBOR: field-order independent, `-0.0` → `0.0`, `NaN` rejected at
/// construction, stable across builds. **A3 fills the internals.**
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ParamBlock {}

impl ParamBlock {
    /// Construct from a recipe fragment against `schema` — validates types,
    /// rejects `NaN`, normalizes `-0.0`, and canonicalizes (spec §3.5).
    pub fn from_canonical_cbor(
        bytes: &[u8],
        schema: &ParamsSchema,
    ) -> Result<ParamBlock, NodeError> {
        let _ = (bytes, schema);
        unimplemented!("A3 (A-core): ParamBlock construction + validation + canonicalization")
    }

    /// The canonical-CBOR bytes (field-order independent). Stable across builds.
    pub fn canonical_bytes(&self) -> &[u8] {
        unimplemented!("A3 (A-core): ParamBlock::canonical_bytes")
    }

    /// blake3 over the canonical bytes — the `param_hash` cache-key ingredient.
    pub fn hash(&self) -> ParamHash {
        unimplemented!("B1: ParamHash over canonical ParamBlock")
    }
}

/// blake3 over a canonical [`ParamBlock`] (spec §3.5 `param_hash`). Equal params
/// ⇒ equal hash across processes/builds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ParamHash(pub blake3::Hash);
