// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `NodeRegistry` + `PvRange` — append-only `(NodeId, pv)` → factory map
//! (spec §3.3).
//!
//! Owner: **A-core** (task **A5**: append-only registration, overlap detection,
//! `resolve`, `supported_pvs`; unit tests for overlapping-range rejection and
//! range-edge resolution).

use std::sync::Arc;

use super::{NodeFactory, RenderNode};
use crate::ng::error::RegistryError;
use crate::ng::types::{NodeId, ProcessVersion};

/// A closed-below, optionally-open-above process-version range (spec §3.3).
#[derive(Clone, Copy, Debug)]
pub struct PvRange {
    /// First process version the factory serves (inclusive).
    pub from: ProcessVersion,
    /// Last process version served (inclusive); `None` = open-ended.
    pub to_inclusive: Option<ProcessVersion>,
}

/// Append-only registry keyed `(NodeId, pv)` (spec §3.3). Old PVs are never
/// replaced — improving an algorithm means a new [`RenderNode`] impl registered
/// for a **new** PV range (§4.5). **A5 fills the internals.**
#[derive(Default)]
pub struct NodeRegistry {}

impl NodeRegistry {
    /// An empty registry.
    pub fn new() -> NodeRegistry {
        NodeRegistry::default()
    }

    /// Registers `f` for `id` across `pvs`. Registering an overlapping
    /// `(id, pv)` range is an error (spec §3.3 — old PVs never replaced).
    pub fn register(
        &mut self,
        id: NodeId,
        pvs: PvRange,
        f: Arc<dyn NodeFactory>,
    ) -> Result<(), RegistryError> {
        let _ = (id, pvs, f);
        unimplemented!("A5 (A-core): NodeRegistry::register — append-only + overlap detection")
    }

    /// Resolves the node registered for `(id, pv)`.
    pub fn resolve(&self, id: NodeId, pv: ProcessVersion) -> Option<Arc<dyn RenderNode>> {
        let _ = (id, pv);
        unimplemented!("A5 (A-core): NodeRegistry::resolve")
    }

    /// Every process version some registered factory serves.
    pub fn supported_pvs(&self) -> Vec<ProcessVersion> {
        unimplemented!("A5 (A-core): NodeRegistry::supported_pvs")
    }
}
