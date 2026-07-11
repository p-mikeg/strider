//! Neighborhood BFS over the CFG region graph, for the interactive explorer.

use petgraph::Direction;
use petgraph::graph::NodeIndex;
use rustc_hash::FxHashSet;
use std::collections::VecDeque;

use crate::Cfg;

/// BFS the depth-`depth` neighborhood around `center` over **both** edge
/// directions (predecessor + successor blocks), capped at `max_nodes`. BFS
/// visits in level order, so the budget keeps the nearest `max_nodes` regions.
// Not yet called outside this module's tests: the DOT renderer and Python
// bindings that consume it land in later tasks of the CFG-explorer feature.
#[allow(dead_code)]
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
        assert!(d1.len() >= 2, "depth 1 must reach a successor: {}", d1.len());
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
}
