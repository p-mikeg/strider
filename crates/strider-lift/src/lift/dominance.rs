use graph_algorithms::dominance::{Dominators, Frontiers};
use petgraph::Direction::Incoming;
use strider_cfg::{Cfg, RegionId};

pub(crate) struct DomInfo {
    pub(crate) doms: Dominators<RegionId>,
    /// Where a definition in `r` first stops dominating, hence where a phi may
    /// be needed.  An absent key means an empty frontier.
    pub(crate) frontiers: Frontiers<RegionId>,
    /// The order the SSA renaming walk uses: every region appears after its
    /// immediate dominator, siblings ascending by `RegionId`. It is the order
    /// the lift translates regions in, so every body node's NodeId follows it.
    preorder: Vec<RegionId>,
}

impl DomInfo {
    #[must_use]
    pub(crate) fn compute(cfg: &Cfg) -> Self {
        let graph = cfg.region_graph();
        let doms = graph_algorithms::dominance::dominators(cfg.entry(), |r| graph.neighbors(r));
        let frontiers = graph_algorithms::dominance::dominance_frontiers(&doms, |r| {
            graph.neighbors_directed(r, Incoming)
        });
        let preorder = doms.tree_preorder();
        Self {
            doms,
            frontiers,
            preorder,
        }
    }

    /// Excludes regions unreachable from the entry.
    #[must_use]
    pub(crate) fn preorder(&self) -> &[RegionId] {
        &self.preorder
    }
}

#[cfg(test)]
mod tests {
    use super::DomInfo;

    /// `test edi,edi ; je 0x100a ; mov eax,1 ; ret ; xor eax,eax ; ret`. The
    /// builder creates the two arms in queue order, and petgraph lists a
    /// region's out-edges newest first, so the depth-first discovery reaches
    /// the arm with the higher `RegionId` first. The SSA walk, whose order
    /// the body nodes' NodeIds follow, takes siblings by `RegionId`.
    #[test]
    fn the_ssa_walk_takes_sibling_regions_by_region_id() {
        let arch = strider_target::SleighArch::x86_64();
        let code = vec![
            0x85, 0xff, 0x74, 0x06, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0x31, 0xc0, 0xc3,
        ];
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let mut lifter = crate::lift::Lifter::new(arch, reader).expect("lifter");
        let cfg = lifter
            .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
            .expect("cfg");
        let entry = cfg.entry();
        let mut arms: Vec<strider_cfg::RegionId> =
            cfg.region_ids().filter(|&r| r != entry).collect();
        arms.sort_unstable();
        assert_eq!(arms.len(), 2, "premise: an entry and two arms");
        let dom = DomInfo::compute(&cfg);
        assert_eq!(
            dom.doms.vertices(),
            [entry, arms[1], arms[0]],
            "premise: discovery reaches the later arm first"
        );
        assert_eq!(dom.preorder(), [entry, arms[0], arms[1]]);
    }
}
