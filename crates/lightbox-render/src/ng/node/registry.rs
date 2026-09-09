// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `NodeRegistry` + `PvRange`, append-only `(NodeId, pv)` → factory map
//! (spec §3.3).
//!
//! Owner: **A-core** (task **A5**: append-only registration, overlap detection,
//! `resolve`, `supported_pvs`; unit tests for overlapping-range rejection and
//! range-edge resolution).

use std::collections::HashMap;
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

impl PvRange {
    /// A range serving a single process version.
    pub fn single(pv: ProcessVersion) -> PvRange {
        PvRange {
            from: pv,
            to_inclusive: Some(pv),
        }
    }

    /// A range serving `from` and every later version (open-ended).
    pub fn from_open(from: ProcessVersion) -> PvRange {
        PvRange {
            from,
            to_inclusive: None,
        }
    }

    /// The effective inclusive upper bound (`u16::MAX` when open).
    fn upper(&self) -> u16 {
        self.to_inclusive.map(|p| p.0).unwrap_or(u16::MAX)
    }

    /// Whether `pv` falls within this range.
    fn contains(&self, pv: ProcessVersion) -> bool {
        pv.0 >= self.from.0 && pv.0 <= self.upper()
    }

    /// Whether the lower bound is above the upper bound (an invalid range).
    fn is_inverted(&self) -> bool {
        self.from.0 > self.upper()
    }

    /// Whether two ranges share any process version.
    fn overlaps(&self, other: &PvRange) -> bool {
        self.from.0 <= other.upper() && other.from.0 <= self.upper()
    }
}

/// The registered PV ranges for one node id (append-only; each an
/// `(range, factory)` pair, insertion order preserved).
type PvEntries = Vec<(PvRange, Arc<dyn NodeFactory>)>;

/// Append-only registry keyed `(NodeId, pv)` (spec §3.3). Old PVs are never
/// replaced, improving an algorithm means a new [`RenderNode`] impl registered
/// for a **new** PV range (§4.5).
#[derive(Default)]
pub struct NodeRegistry {
    entries: HashMap<NodeId, PvEntries>,
}

impl NodeRegistry {
    /// An empty registry.
    pub fn new() -> NodeRegistry {
        NodeRegistry::default()
    }

    /// Registers `f` for `id` across `pvs`. Registering an overlapping
    /// `(id, pv)` range is an error (spec §3.3, old PVs never replaced).
    pub fn register(
        &mut self,
        id: NodeId,
        pvs: PvRange,
        f: Arc<dyn NodeFactory>,
    ) -> Result<(), RegistryError> {
        if pvs.is_inverted() {
            return Err(RegistryError::OverlappingPv {
                id,
                detail: format!("inverted range: from {} > to {}", pvs.from.0, pvs.upper()),
            });
        }
        let ranges = self.entries.entry(id).or_default();
        if let Some((existing, _)) = ranges.iter().find(|(r, _)| r.overlaps(&pvs)) {
            return Err(RegistryError::OverlappingPv {
                id,
                detail: format!(
                    "new range [{}, {}] overlaps existing [{}, {}]",
                    pvs.from.0,
                    pvs.upper(),
                    existing.from.0,
                    existing.upper()
                ),
            });
        }
        ranges.push((pvs, f));
        Ok(())
    }

    /// Resolves the node registered for `(id, pv)`.
    pub fn resolve(&self, id: NodeId, pv: ProcessVersion) -> Option<Arc<dyn RenderNode>> {
        self.entries
            .get(&id)?
            .iter()
            .find(|(r, _)| r.contains(pv))
            .map(|(_, f)| f.instantiate())
    }

    /// Whether some factory serves `(id, pv)` (topology resolution without
    /// instantiating).
    pub fn contains(&self, id: NodeId, pv: ProcessVersion) -> bool {
        self.entries
            .get(&id)
            .is_some_and(|ranges| ranges.iter().any(|(r, _)| r.contains(pv)))
    }

    /// The kernel salt of the factory serving `(id, pv)`, if registered
    /// (the cache-key ingredient, spec §3.5). Never instantiates the node.
    pub fn kernel_salt(&self, id: NodeId, pv: ProcessVersion) -> Option<super::KernelSalt> {
        self.entries
            .get(&id)?
            .iter()
            .find(|(r, _)| r.contains(pv))
            .map(|(_, f)| f.kernel_salt())
    }

    /// The distinct lower bounds of every registered range, sorted, the
    /// discrete process versions this registry can serve entry-points for.
    pub fn supported_pvs(&self) -> Vec<ProcessVersion> {
        let mut pvs: Vec<u16> = self
            .entries
            .values()
            .flat_map(|ranges| ranges.iter().map(|(r, _)| r.from.0))
            .collect();
        pvs.sort_unstable();
        pvs.dedup();
        pvs.into_iter().map(ProcessVersion).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::node::{KernelSalt, NodeDescriptor, ParamsSchema, ParamsSchemaRef};
    use crate::ng::tile::{CpuTileView, TileView};
    use crate::ng::{CpuEvalCtx, GpuEvalCtx, NodeError, ParamBlock, PortDecl, PortType};

    static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;
    static DESC: NodeDescriptor = NodeDescriptor {
        id: NodeId("test.reg"),
        inputs: &[],
        output: PortDecl {
            name: "out",
            ty: PortType::LinearRgbaF16,
        },
        params_schema: ParamsSchemaRef(&SCHEMA),
    };

    struct DummyNode;
    impl RenderNode for DummyNode {
        fn descriptor(&self) -> &NodeDescriptor {
            &DESC
        }
        fn eval_gpu(
            &self,
            _: &mut GpuEvalCtx<'_>,
            _: &[TileView<'_>],
            _: &ParamBlock,
        ) -> Result<(), NodeError> {
            Ok(())
        }
        fn eval_cpu(
            &self,
            _: &mut CpuEvalCtx<'_>,
            _: &[CpuTileView<'_>],
            _: &ParamBlock,
        ) -> Result<(), NodeError> {
            Ok(())
        }
    }

    struct DummyFactory(u8);
    impl NodeFactory for DummyFactory {
        fn instantiate(&self) -> Arc<dyn RenderNode> {
            Arc::new(DummyNode)
        }
        fn kernel_salt(&self) -> KernelSalt {
            KernelSalt(blake3::hash(&[self.0]))
        }
    }

    fn pv(n: u16) -> ProcessVersion {
        ProcessVersion(n)
    }

    #[test]
    fn resolves_within_range_and_at_edges() {
        let mut reg = NodeRegistry::new();
        reg.register(
            NodeId("a"),
            PvRange {
                from: pv(1),
                to_inclusive: Some(pv(3)),
            },
            Arc::new(DummyFactory(1)),
        )
        .unwrap();
        assert!(reg.resolve(NodeId("a"), pv(1)).is_some()); // low edge
        assert!(reg.resolve(NodeId("a"), pv(2)).is_some());
        assert!(reg.resolve(NodeId("a"), pv(3)).is_some()); // high edge
        assert!(reg.resolve(NodeId("a"), pv(4)).is_none()); // just past
        assert!(reg.resolve(NodeId("a"), pv(0)).is_none()); // just below
        assert!(reg.resolve(NodeId("missing"), pv(1)).is_none());
    }

    #[test]
    fn open_range_resolves_all_later_pvs() {
        let mut reg = NodeRegistry::new();
        reg.register(
            NodeId("a"),
            PvRange::from_open(pv(2)),
            Arc::new(DummyFactory(1)),
        )
        .unwrap();
        assert!(reg.resolve(NodeId("a"), pv(1)).is_none());
        assert!(reg.resolve(NodeId("a"), pv(2)).is_some());
        assert!(reg.resolve(NodeId("a"), pv(999)).is_some());
    }

    #[test]
    fn overlapping_registration_is_rejected() {
        let mut reg = NodeRegistry::new();
        reg.register(
            NodeId("a"),
            PvRange::from_open(pv(1)),
            Arc::new(DummyFactory(1)),
        )
        .unwrap();
        // [2,3] overlaps the open [1, ..].
        let err = reg
            .register(
                NodeId("a"),
                PvRange {
                    from: pv(2),
                    to_inclusive: Some(pv(3)),
                },
                Arc::new(DummyFactory(2)),
            )
            .unwrap_err();
        assert!(matches!(err, RegistryError::OverlappingPv { .. }));
    }

    #[test]
    fn adjacent_non_overlapping_ranges_coexist() {
        let mut reg = NodeRegistry::new();
        reg.register(
            NodeId("a"),
            PvRange {
                from: pv(1),
                to_inclusive: Some(pv(3)),
            },
            Arc::new(DummyFactory(1)),
        )
        .unwrap();
        reg.register(
            NodeId("a"),
            PvRange::from_open(pv(4)),
            Arc::new(DummyFactory(2)),
        )
        .unwrap();
        assert!(reg.resolve(NodeId("a"), pv(3)).is_some());
        assert!(reg.resolve(NodeId("a"), pv(4)).is_some());
        // Distinct kernel salts prove the range edges resolve to distinct impls.
        assert_ne!(
            reg.kernel_salt(NodeId("a"), pv(3)),
            reg.kernel_salt(NodeId("a"), pv(4))
        );
    }

    #[test]
    fn inverted_range_is_rejected() {
        let mut reg = NodeRegistry::new();
        let err = reg
            .register(
                NodeId("a"),
                PvRange {
                    from: pv(5),
                    to_inclusive: Some(pv(2)),
                },
                Arc::new(DummyFactory(1)),
            )
            .unwrap_err();
        assert!(matches!(err, RegistryError::OverlappingPv { .. }));
    }

    #[test]
    fn supported_pvs_lists_distinct_lower_bounds() {
        let mut reg = NodeRegistry::new();
        reg.register(
            NodeId("a"),
            PvRange::from_open(pv(1)),
            Arc::new(DummyFactory(1)),
        )
        .unwrap();
        reg.register(
            NodeId("b"),
            PvRange::single(pv(999)),
            Arc::new(DummyFactory(2)),
        )
        .unwrap();
        assert_eq!(reg.supported_pvs(), vec![pv(1), pv(999)]);
    }
}
