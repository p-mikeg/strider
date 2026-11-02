use petgraph::Direction;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;

use crate::Cfg;

/// Walks BOTH edge directions, so predecessors and successors alike.  BFS
/// visits in level order, so the `max_nodes` budget keeps the nearest regions.
///
/// `center` is in the set before the budget is consulted, so the result holds
/// at least it however small `max_nodes` is, and at most `max(1, max_nodes)`
/// regions.
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
    /// Pretty render around `center`.  Node ids are region indices; `center`
    /// gets a gold border, and is rendered whatever `max_nodes` is.
    ///
    /// # Errors
    /// `center` is not a region of this CFG, or the render fails.
    pub fn neighborhood_dot(
        &self,
        names: &strider_target::ArchNames,
        center: NodeIndex,
        depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        if self.region_graph.node_weight(center).is_none() {
            anyhow::bail!("invalid region id {}", center.index());
        }
        let set = neighborhood_regions(self, center, depth, max_nodes);
        // The full-CFG dumper restricted to `set`: same labels, and the
        // if-true / if-false edge polarity a second emitter here did not carry.
        let view = ::dot::View::new(set, center);
        let dumper = self.dot_dumper(names).restricted(view);
        ::dot::GraphDot::new(dumper, ::dot::DotStyle::dark_cfg()).as_dot()
    }
}

#[cfg(test)]
mod tests {
    use super::neighborhood_regions;
    use rsleigh::mem_readers::BufMemReader;
    use strider_target::SleighArch;

    use crate::CfgOptions;

    // `jne +1`, `nop`, `ret`: an entry block plus two successors.
    fn two_way_cfg() -> crate::Cfg {
        let bytes = vec![0x75, 0x01, 0x90, 0xc3];
        let start = 0x1000;
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, start);
        crate::DecodeEngine::new(arch, reader)
            .expect("decode engine")
            .build_cfg(start.into(), &CfgOptions::default())
            .expect("build_cfg on synthetic bytes")
    }

    #[test]
    fn depth_bounds_and_walks_both_directions() {
        use petgraph::Direction;

        let cfg = two_way_cfg();
        let entry = cfg.entry();
        assert_eq!(neighborhood_regions(&cfg, entry, 0, 999).len(), 1);
        let d1 = neighborhood_regions(&cfg, entry, 1, 999);
        assert!(
            d1.len() >= 2,
            "depth 1 must reach a successor: {}",
            d1.len()
        );
        assert!(d1.contains(&entry));
        assert!(neighborhood_regions(&cfg, entry, 5, 1).len() <= 1);
        // The centre outranks the budget, so a zero budget still renders it.
        let none_budgeted = neighborhood_regions(&cfg, entry, 5, 0);
        assert_eq!(none_budgeted.len(), 1);
        assert!(none_budgeted.contains(&entry));

        // Exercises the Incoming half: the `ret` block is a confluence of the
        // jne-taken edge and the nop fall-through, so centering there must pull
        // in both predecessors.  Dropping `Incoming` would still pass every
        // entry-centered check above, since `entry` has no predecessors.
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

        // Names only: the CFG renders without the engine that built it.
        let dot = cfg
            .neighborhood_dot(SleighArch::x86_64().names(), entry, 1, 999)
            .expect("neighborhood_dot");
        assert!(
            dot.contains(&format!("\"{}\"", entry.index())),
            "dot:\n{dot}"
        );
        assert!(dot.contains("#ffcc00"), "dot:\n{dot}");
    }

    /// A center the CFG lacks is an error, not an empty digraph.
    #[test]
    fn neighborhood_dot_rejects_a_region_the_cfg_lacks() {
        let cfg = two_way_cfg();
        let missing = crate::RegionId::new(cfg.region_graph().node_count());
        let err = cfg
            .neighborhood_dot(SleighArch::x86_64().names(), missing, 1, 999)
            .expect_err("a missing center must not render");
        assert!(err.to_string().contains("invalid region id"), "{err}");
    }
}
