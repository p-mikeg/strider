use graph_algorithms::dominance::{DomRelation, DomTree, Dominators, Frontiers};
use petgraph::Direction::Incoming;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{Cfg, RegionId};

struct CfgDomTree<'a> {
    cfg: &'a Cfg,
    doms: &'a Dominators<RegionId>,
}

impl DomRelation for CfgDomTree<'_> {
    type Node = RegionId;
    fn nodes(&self) -> impl Iterator<Item = RegionId> + '_ {
        self.cfg.region_ids()
    }
    fn immediate_dominator(&self, n: RegionId) -> Option<RegionId> {
        self.doms.immediate_dominator(n)
    }
}

impl DomTree for CfgDomTree<'_> {
    fn predecessors(&self, n: RegionId) -> impl Iterator<Item = RegionId> + '_ {
        self.cfg.region_graph().neighbors_directed(n, Incoming)
    }
}

pub(crate) struct DomInfo {
    doms: Dominators<RegionId>,
    /// Where a definition in `r` first stops dominating, hence where a phi may
    /// be needed.  An absent key means an empty frontier.
    frontiers: Frontiers<RegionId>,
    /// The order the SSA renaming walk uses: every region appears after its
    /// immediate dominator.
    preorder: Vec<RegionId>,
}

impl DomInfo {
    #[must_use]
    pub(crate) fn compute(cfg: &Cfg) -> Self {
        let graph = cfg.region_graph();
        let doms = graph_algorithms::dominance::dominators(cfg.entry(), |r| graph.neighbors(r));

        // The adapter only borrows `doms`; that borrow ends before `doms` moves
        // into `Self`.
        let (frontiers, preorder) = {
            let adapter = CfgDomTree { cfg, doms: &doms };
            (
                graph_algorithms::dominance::dominance_frontiers(&adapter, cfg.entry()),
                doms.tree_preorder(),
            )
        };

        Self {
            doms,
            frontiers,
            preorder,
        }
    }

    /// `None` for the entry region and for any region unreachable from it.
    #[must_use]
    pub(crate) fn immediate_dominator(&self, r: RegionId) -> Option<RegionId> {
        self.doms.immediate_dominator(r)
    }

    /// Excludes regions unreachable from the entry.
    #[must_use]
    pub(crate) fn preorder(&self) -> &[RegionId] {
        &self.preorder
    }

    /// Cytron phi placement: maps `variable -> defining regions` to the
    /// variables needing a value `Phi` at each region.
    #[must_use]
    pub(crate) fn iterated_frontier<Var, C, H>(
        &self,
        def_sites: &std::collections::HashMap<Var, C, H>,
    ) -> FxHashMap<RegionId, FxHashSet<Var>>
    where
        Var: Copy + Eq + std::hash::Hash,
        H: std::hash::BuildHasher,
        for<'a> &'a C: IntoIterator<Item = &'a RegionId>,
    {
        graph_algorithms::dominance::phi_placement(&self.frontiers, def_sites)
    }
}

#[cfg(test)]
mod tests {
    use super::DomInfo;

    /// `test edi,edi ; je 0x100a ; mov eax,1 ; ret ; xor eax,eax ; ret`. The
    /// builder creates the two arms in queue order, and petgraph lists a
    /// region's out-edges newest first, so the depth-first discovery reaches
    /// the arm with the higher `RegionId` first. The SSA walk, which fixes the
    /// order regions and phis are created in, takes siblings by `RegionId`.
    #[test]
    fn the_ssa_walk_takes_sibling_regions_by_region_id() {
        let arch = strider_target::SleighArch::x86_64();
        let code = vec![
            0x85, 0xff, 0x74, 0x06, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0x31, 0xc0, 0xc3,
        ];
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let mut lifter = crate::lift::Lifter::new(arch, sleigh).expect("lifter");
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
