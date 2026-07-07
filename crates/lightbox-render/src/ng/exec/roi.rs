// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! ROI back-propagation planning (spec §3.2 `plan`; task **C1**).
//!
//! Owner: **C**. To produce an output region the executor must know which input
//! region each node needs — a radius-`r` neighborhood node needs its output
//! grown by `ceil(r * scale)` on every side (the apron). This module walks the
//! compiled [`RenderGraph`] in **reverse** topological order, calling each
//! node's [`RenderNode::plan`], unioning the required output regions, and
//! clamping to each producer's full extent — so the source ROI a tiled render
//! fetches is exactly the terminal ROI grown by the *composed* apron of the
//! chain, and no larger (the C1 minimality property).

use std::collections::HashMap;

use crate::ng::graph::{NodeIndex, RenderGraph};
use crate::ng::types::{Extent, Roi};

/// The forward-computed full output extent of every node, plus the
/// back-propagated required output ROI for one terminal request (task C1).
#[derive(Clone, Debug, Default)]
pub struct RoiPlan {
    /// Full output extent per node (identity-propagated from the source extent;
    /// crop/rotate nodes — E11.4 — change it via `output_extent`).
    pub extents: HashMap<NodeIndex, Extent>,
    /// The output ROI each node must fill to satisfy the terminal request
    /// (clamped to that node's full extent).
    pub out_rois: HashMap<NodeIndex, Roi>,
}

impl RoiPlan {
    /// The required output ROI for `idx`, if the plan reached it.
    pub fn out_roi(&self, idx: NodeIndex) -> Option<Roi> {
        self.out_rois.get(&idx).copied()
    }

    /// The full output extent of `idx`, if computed.
    pub fn extent(&self, idx: NodeIndex) -> Option<Extent> {
        self.extents.get(&idx).copied()
    }
}

/// The half-open bounding box that covers both `a` and `b` (their ROI union).
pub fn bounding_union(a: Roi, b: Roi) -> Roi {
    if a.w == 0 || a.h == 0 {
        return b;
    }
    if b.w == 0 || b.h == 0 {
        return a;
    }
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x as i64 + a.w as i64).max(b.x as i64 + b.w as i64);
    let y1 = (a.y as i64 + a.h as i64).max(b.y as i64 + b.h as i64);
    Roi {
        x: x0,
        y: y0,
        w: (x1 - x0 as i64) as u32,
        h: (y1 - y0 as i64) as u32,
    }
}

/// Clamp `roi` to the `[0, extent)` image rectangle (apron growth past a border
/// is discarded — a neighborhood node edge-replicates there, so no out-of-image
/// pixels are ever fetched).
pub fn clamp_to_extent(roi: Roi, extent: Extent) -> Roi {
    let img = Roi {
        x: 0,
        y: 0,
        w: extent.w,
        h: extent.h,
    };
    roi.intersect(&img).unwrap_or(Roi {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    })
}

/// Compute each node's full output extent, forward through the graph, given the
/// `source_extent` produced by input-less (source) nodes (task C1).
pub fn forward_extents(
    graph: &RenderGraph,
    source_extent: Extent,
) -> Result<HashMap<NodeIndex, Extent>, crate::ng::error::CompileError> {
    let topo = graph.topo_order()?;
    let mut extents: HashMap<NodeIndex, Extent> = HashMap::with_capacity(topo.len());
    for idx in topo {
        let node = graph.node(idx);
        let input_idxs = graph.inputs_of(idx);
        let ext = if input_idxs.is_empty() {
            source_extent
        } else {
            let in_extents: Vec<Extent> = input_idxs
                .iter()
                .map(|&src| extents.get(&src).copied().unwrap_or(source_extent))
                .collect();
            node.output_extent(&in_extents, graph.params(idx))
        };
        extents.insert(idx, ext);
    }
    Ok(extents)
}

/// Back-propagate the `terminal_out_roi` required at the graph's single sink to
/// an output ROI for every node, at decimation `scale` (task C1).
///
/// Reverse topological order guarantees a node's full set of downstream
/// consumers is resolved before we compute its required output ROI. Each node's
/// [`RenderNode::plan`] maps its required output ROI to per-input ROIs; those
/// are unioned into the producing upstream node's requirement and clamped to the
/// producer's full extent.
pub fn plan_rois(
    graph: &RenderGraph,
    terminal_out_roi: Roi,
    scale: f32,
    source_extent: Extent,
) -> Result<RoiPlan, crate::ng::error::CompileError> {
    let extents = forward_extents(graph, source_extent)?;
    let topo = graph.topo_order()?;

    let sink = graph
        .single_sink()
        .ok_or(crate::ng::error::CompileError::Cyclic)?;

    let mut out_rois: HashMap<NodeIndex, Roi> = HashMap::with_capacity(topo.len());
    let sink_ext = extents.get(&sink).copied().unwrap_or(source_extent);
    out_rois.insert(sink, clamp_to_extent(terminal_out_roi, sink_ext));

    // Walk sinks→sources: every consumer of `idx` is visited before `idx`.
    for &idx in topo.iter().rev() {
        let Some(&out_roi) = out_rois.get(&idx) else {
            // Unreached (a disconnected component not feeding the sink).
            continue;
        };
        let node = graph.node(idx);
        let input_rois = node.plan(out_roi, scale, graph.params(idx)).0;
        let upstream = graph.inputs_of(idx); // connected inputs, in declared-port order
        for (port_i, &src) in upstream.iter().enumerate() {
            // A node with fewer declared plan ROIs than connected inputs (should
            // not happen for our nodes) falls back to identity on that port.
            let needed = input_rois.get(port_i).copied().unwrap_or(out_roi);
            let src_ext = extents.get(&src).copied().unwrap_or(source_extent);
            let clamped = clamp_to_extent(needed, src_ext);
            let merged = match out_rois.get(&src) {
                Some(&prev) => bounding_union(prev, clamped),
                None => clamped,
            };
            out_rois.insert(src, merged);
        }
    }

    Ok(RoiPlan { extents, out_rois })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::node::param::ParamsSchema;
    use crate::ng::node::{
        CpuEvalCtx, GpuEvalCtx, InputRois, NodeDescriptor, ParamBlock, ParamsSchemaRef, RenderNode,
    };
    use crate::ng::tile::{CpuTileView, TileView};
    use crate::ng::types::{NodeId, PortType};
    use crate::ng::{NodeError, PortDecl};
    use std::sync::Arc;

    static SCHEMA: ParamsSchema = ParamsSchema::EMPTY;

    fn roi(x: i32, y: i32, w: u32, h: u32) -> Roi {
        Roi { x, y, w, h }
    }

    /// A source node (no inputs).
    struct Src {
        desc: NodeDescriptor,
    }
    /// A radius-`r` neighborhood node: `plan` grows the output by `ceil(r*scale)`.
    struct Blur {
        desc: NodeDescriptor,
        radius: u32,
    }

    impl RenderNode for Src {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.desc
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

    impl RenderNode for Blur {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.desc
        }
        fn plan(&self, out: Roi, scale: f32, _: &ParamBlock) -> InputRois {
            let margin = (self.radius as f32 * scale).ceil() as u32;
            InputRois(vec![out.expand(margin)])
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

    static IN_F16: &[PortDecl] = &[PortDecl {
        name: "in",
        ty: PortType::LinearRgbaF16,
    }];

    fn src(id: &'static str) -> Arc<dyn RenderNode> {
        Arc::new(Src {
            desc: NodeDescriptor {
                id: NodeId(id),
                inputs: &[],
                output: PortDecl {
                    name: "out",
                    ty: PortType::LinearRgbaF16,
                },
                params_schema: ParamsSchemaRef(&SCHEMA),
            },
        })
    }
    fn blur(id: &'static str, radius: u32) -> Arc<dyn RenderNode> {
        Arc::new(Blur {
            desc: NodeDescriptor {
                id: NodeId(id),
                inputs: IN_F16,
                output: PortDecl {
                    name: "out",
                    ty: PortType::LinearRgbaF16,
                },
                params_schema: ParamsSchemaRef(&SCHEMA),
            },
            radius,
        })
    }

    #[test]
    fn bounding_union_covers_both() {
        let u = bounding_union(roi(0, 0, 10, 10), roi(5, 5, 10, 10));
        assert_eq!(u, roi(0, 0, 15, 15));
        // Union with an empty ROI is identity.
        assert_eq!(
            bounding_union(roi(2, 3, 4, 5), roi(0, 0, 0, 0)),
            roi(2, 3, 4, 5)
        );
    }

    #[test]
    fn clamp_discards_out_of_image_apron() {
        let c = clamp_to_extent(roi(-4, -4, 20, 20), Extent { w: 10, h: 10 });
        assert_eq!(c, roi(0, 0, 10, 10));
    }

    /// **C1 composition property:** through `src → blur(r1) → blur(r2) → sink`,
    /// the source ROI is the terminal ROI grown by the *sum* of aprons, and no
    /// larger (minimal), clamped to the image.
    #[test]
    fn composed_aprons_sum_and_are_minimal() {
        let mut g = RenderGraph::new();
        let s = g.add_node(src("src"));
        let b1 = g.add_node(blur("blur1", 3));
        let b2 = g.add_node(blur("blur2", 5));
        g.connect(s, b1, "in").unwrap();
        g.connect(b1, b2, "in").unwrap();
        let source_extent = Extent { w: 1000, h: 1000 };

        // Ask for an interior 100×100 output region at scale 1.0.
        let want = roi(400, 400, 100, 100);
        let plan = plan_rois(&g, want, 1.0, source_extent).unwrap();

        // blur2 needs its output grown by 5; blur1 needs that grown by 3.
        assert_eq!(plan.out_roi(b2).unwrap(), want);
        assert_eq!(plan.out_roi(b1).unwrap(), roi(395, 395, 110, 110));
        // Source: grown by 5 + 3 = 8 on every side → minimal.
        assert_eq!(plan.out_roi(s).unwrap(), roi(392, 392, 116, 116));
    }

    #[test]
    fn apron_scales_with_decimation_and_clamps_at_border() {
        let mut g = RenderGraph::new();
        let s = g.add_node(src("src"));
        let b = g.add_node(blur("blur", 4));
        g.connect(s, b, "in").unwrap();
        let ext = Extent { w: 64, h: 64 };

        // At scale 0.5 the radius-4 apron is ceil(4*0.5)=2. A corner output tile
        // clamps its apron to the image (no negative origin fetched).
        let plan = plan_rois(&g, roi(0, 0, 16, 16), 0.5, ext).unwrap();
        assert_eq!(plan.out_roi(s).unwrap(), roi(0, 0, 18, 18)); // grown by 2, clamped left/top
    }

    #[test]
    fn identity_chain_needs_exactly_the_requested_roi() {
        // A point-wise chain (default identity plan) never grows the ROI.
        let mut g = RenderGraph::new();
        let s = g.add_node(src("src"));
        let a = g.add_node(blur("noop", 0)); // radius 0 → identity apron
        g.connect(s, a, "in").unwrap();
        let plan = plan_rois(&g, roi(10, 20, 30, 40), 1.0, Extent { w: 100, h: 100 }).unwrap();
        assert_eq!(plan.out_roi(s).unwrap(), roi(10, 20, 30, 40));
    }
}
