//! Neighborhood BFS over the CFG region graph, for the interactive explorer.

use petgraph::Direction;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;

use crate::Cfg;

/// BFS the depth-`depth` neighborhood around `center` over **both** edge
/// directions (predecessor + successor blocks), capped at `max_nodes`. BFS
/// visits in level order, so the budget keeps the nearest `max_nodes` regions.
pub(crate) fn neighborhood_regions(
    cfg: &Cfg,
    center: NodeIndex,
    depth: usize,
    max_nodes: usize,
) -> FxHashSet<NodeIndex> {
    let g = cfg.region_graph();
    let mut seen = FxHashSet::default();
    seen.insert(center);
    let mut queue = VecDeque::from([(center, 0usize)]);
    'bfs: while let Some((node, dist)) = queue.pop_front() {
        if dist >= depth {
            continue;
        }
        let neighbors = g
            .neighbors_directed(node, Direction::Incoming)
            .chain(g.neighbors_directed(node, Direction::Outgoing));
        for nb in neighbors {
            if seen.len() >= max_nodes {
                break 'bfs;
            }
            if seen.insert(nb) {
                queue.push_back((nb, dist + 1));
            }
        }
    }
    seen
}

impl Cfg {
    /// Pretty render of the depth-`depth` neighborhood around region
    /// `center` (BFS over predecessor+successor blocks, `max_nodes`
    /// budget), reusing the full-CFG block styling (see
    /// `CfgDotDumper::dump_as_dot` in `dot.rs`, whose per-block label
    /// logic this mirrors). DOT node ids are region indices; `center`
    /// gets a gold border.
    ///
    /// `::dot` (leading `::`) is required here, not a plain `dot::` path:
    /// this crate also has a private sibling module named `dot`
    /// (`crate::dot`), so an unqualified `dot::` path would be ambiguous
    /// between that module and the external `dot` crate.
    ///
    /// # Errors
    /// Returns an error if `center` (or any region reachable within the
    /// neighborhood) is missing from the graph, or if resolving the
    /// Sleigh register table fails.
    pub fn neighborhood_dot<R: rsleigh::MemReader>(
        &self,
        sleigh: &rsleigh::Sleigh<R>,
        center: NodeIndex,
        depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        let set = neighborhood_regions(self, center, depth, max_nodes);
        let regs = sleigh.regs()?;
        let g = self.region_graph();
        let mut out = ::dot::DotEmitter::new("G", &::dot::DotStyle::dark_cfg());
        for &node in &set {
            let region = g
                .node_weight(node)
                .ok_or_else(|| anyhow::anyhow!("invalid region index {node:?}"))?;
            let start = region.start_addr.machine_addr.addr;
            let mut label = format!("Instruction(addr={start:#x})");
            for insn in &region.insns {
                let a = insn.addr.machine_addr.addr;
                let pretty = insn.insn.ctx_fmt(sleigh, &regs);
                label.push_str(&format!("\\l{a:#x}: {pretty}"));
            }
            label.push_str("\\l");
            let id = node.index().to_string();
            let extra: &[(&str, &str)] = if node == center {
                &[("color", "\"#ffcc00\""), ("penwidth", "2.5")]
            } else {
                &[]
            };
            out.node(&id, &label, "box", extra);
        }
        // ponytail: v1 simplification — control edges within the
        // neighborhood are plain (no if-true/if-false labels like the
        // full-CFG dumper). Recovering that polarity here means resolving
        // `region_if` per source, which is a follow-up if the explorer
        // needs it.
        for &node in &set {
            for succ in g.neighbors_directed(node, Direction::Outgoing) {
                if set.contains(&succ) {
                    out.edge(&node.index().to_string(), &succ.index().to_string(), &[]);
                }
            }
        }
        Ok(out.finish())
    }

    /// Structure-faithful render of the neighborhood: one `n<idx>` box per
    /// region (start addr + instruction count), edges as stored, no Sleigh.
    ///
    /// # Errors
    /// Returns an error if `center` (or any region reachable within the
    /// neighborhood) is missing from the graph.
    pub fn raw_neighborhood_dot(
        &self,
        center: NodeIndex,
        depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        let set = neighborhood_regions(self, center, depth, max_nodes);
        let g = self.region_graph();
        let mut out = ::dot::DotEmitter::new("G", &::dot::DotStyle::dark_cfg());
        for &node in &set {
            let region = g
                .node_weight(node)
                .ok_or_else(|| anyhow::anyhow!("invalid region index {node:?}"))?;
            let start = region.start_addr.machine_addr.addr;
            let label = format!(
                "n{}  {start:#x}\\l{} insns",
                node.index(),
                region.insns.len()
            );
            let id = format!("n{}", node.index());
            let extra: &[(&str, &str)] = if node == center {
                &[("color", "\"#ffcc00\""), ("penwidth", "2.5")]
            } else {
                &[]
            };
            out.node(&id, &label, "box", extra);
        }
        // ponytail: v1 simplification — same plain-edge note as
        // `neighborhood_dot` above.
        for &node in &set {
            for succ in g.neighbors_directed(node, Direction::Outgoing) {
                if set.contains(&succ) {
                    out.edge(
                        &format!("n{}", node.index()),
                        &format!("n{}", succ.index()),
                        &[],
                    );
                }
            }
        }
        Ok(out.finish())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::neighborhood_regions;
    use rsleigh::mem_readers::BufMemReader;
    use strider_target::SleighArch;

    use crate::{Builder, CfgOptions};

    // Two x86_64 basic blocks: `jz` splits into a taken/fallthrough pair.
    // 7500 (jz +2), 90 (nop), C3 (ret) -> entry block + two successors.
    fn two_way_cfg() -> crate::Cfg {
        let bytes = vec![0x75, 0x01, 0x90, 0xc3];
        let start = 0x1000;
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, start);
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
        Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
            .build()
            .expect("Builder::build on synthetic bytes")
    }

    #[test]
    fn depth_bounds_and_walks_both_directions() {
        use petgraph::Direction;

        let cfg = two_way_cfg();
        let entry = cfg.entry();
        // depth 0 = just the center
        assert_eq!(neighborhood_regions(&cfg, entry, 0, 999).len(), 1);
        // depth 1 from entry reaches its successor block(s)
        let d1 = neighborhood_regions(&cfg, entry, 1, 999);
        assert!(
            d1.len() >= 2,
            "depth 1 must reach a successor: {}",
            d1.len()
        );
        assert!(d1.contains(&entry));
        // budget caps the set
        assert!(neighborhood_regions(&cfg, entry, 5, 1).len() <= 1);

        // Observe the Incoming half of the traversal: the `ret` block is a
        // confluence reached by BOTH the jz-taken edge and the nop
        // fallthrough, so it has >=2 predecessors.  Centering the depth-1
        // neighborhood there must pull in each predecessor via `Incoming`
        // (a regression that dropped `Incoming` would fail here but pass the
        // entry-centered checks above, since `entry` has no predecessors).
        let g = cfg.region_graph();
        let confluence = g
            .node_indices()
            .find(|&n| g.neighbors_directed(n, Direction::Incoming).count() >= 2)
            .expect("two-way fixture must have a >=2-predecessor confluence region");
        let preds: Vec<_> = g
            .neighbors_directed(confluence, Direction::Incoming)
            .collect();
        let around = neighborhood_regions(&cfg, confluence, 1, 999);
        for pred in preds {
            assert!(
                around.contains(&pred),
                "depth-1 neighborhood of the confluence must include predecessor {pred:?} \
                 (proves the Incoming traversal fires): {around:?}"
            );
        }
    }

    #[test]
    fn neighborhood_dot_ids_are_region_indices_and_center_highlighted() {
        let cfg = two_way_cfg();
        let entry = cfg.entry();

        // Fresh Sleigh for the render call, matching the `dot_string` harness
        // in `dot.rs` (the CFG doesn't own the Sleigh that built it).
        let bytes = vec![0x75, 0x01, 0x90, 0xc3];
        let start = 0x1000;
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, start);
        let sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");

        let dot = cfg
            .neighborhood_dot(&sleigh, entry, 1, 999)
            .expect("neighborhood_dot");
        // real dot node id == region index of the center
        assert!(
            dot.contains(&format!("\"{}\"", entry.index())),
            "dot:\n{dot}"
        );
        // center carries the gold highlight border
        assert!(dot.contains("#ffcc00"), "dot:\n{dot}");

        // raw: one n<idx> box per region, no Sleigh, edges as stored
        let raw = cfg
            .raw_neighborhood_dot(entry, 1, 999)
            .expect("raw_neighborhood_dot");
        assert!(raw.contains(&format!("n{}", entry.index())), "raw:\n{raw}");
    }
}
