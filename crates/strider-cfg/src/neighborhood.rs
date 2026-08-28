use petgraph::Direction;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;

use crate::Cfg;

/// Walks BOTH edge directions, so predecessors and successors alike.  BFS
/// visits in level order, so the `max_nodes` budget keeps the nearest regions.
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
    /// gets a gold border.
    pub fn neighborhood_dot<R: rsleigh::MemReader>(
        &self,
        sleigh: &rsleigh::Sleigh<R>,
        center: NodeIndex,
        depth: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        let set = neighborhood_regions(self, center, depth, max_nodes);
        // The full-CFG dumper restricted to `set`: same labels, and the
        // if-true / if-false edge polarity a second emitter here did not carry.
        let dumper = self.dot_dumper(sleigh).restricted(&set, center);
        ::dot::GraphDot::new(dumper, ::dot::DotStyle::dark_cfg()).as_dot()
    }
}

#[cfg(test)]
mod tests {
    use super::neighborhood_regions;
    use rsleigh::mem_readers::BufMemReader;
    use strider_target::SleighArch;

    use crate::{Builder, CfgOptions};

    // `jz +2`, `nop`, `ret`: an entry block plus two successors.
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
        assert_eq!(neighborhood_regions(&cfg, entry, 0, 999).len(), 1);
        let d1 = neighborhood_regions(&cfg, entry, 1, 999);
        assert!(
            d1.len() >= 2,
            "depth 1 must reach a successor: {}",
            d1.len()
        );
        assert!(d1.contains(&entry));
        assert!(neighborhood_regions(&cfg, entry, 5, 1).len() <= 1);

        // Exercises the Incoming half: the `ret` block is a confluence of the
        // jz-taken edge and the nop fall-through, so centering there must pull
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

        // A fresh Sleigh: the CFG does not own the one that built it.
        let bytes = vec![0x75, 0x01, 0x90, 0xc3];
        let start = 0x1000;
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, start);
        let sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");

        let dot = cfg
            .neighborhood_dot(&sleigh, entry, 1, 999)
            .expect("neighborhood_dot");
        assert!(
            dot.contains(&format!("\"{}\"", entry.index())),
            "dot:\n{dot}"
        );
        assert!(dot.contains("#ffcc00"), "dot:\n{dot}");
    }
}
