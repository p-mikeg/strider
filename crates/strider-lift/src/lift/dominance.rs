use graph_algorithms::dominance::{DefSites, DomTree, Frontiers};
use petgraph::Direction::Incoming;
use petgraph::algo::dominators::{Dominators, simple_fast};
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{Cfg, RegionId};

struct CfgDomTree<'a> {
    cfg: &'a Cfg,
    doms: &'a Dominators<RegionId>,
}

impl DomTree for CfgDomTree<'_> {
    type Node = RegionId;
    fn nodes(&self) -> impl Iterator<Item = RegionId> + '_ {
        self.cfg.region_ids()
    }
    fn predecessors(&self, n: RegionId) -> impl Iterator<Item = RegionId> + '_ {
        self.cfg.region_graph().neighbors_directed(n, Incoming)
    }
    fn immediate_dominator(&self, n: RegionId) -> Option<RegionId> {
        self.doms.immediate_dominator(n)
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
        let doms = simple_fast(cfg.region_graph(), cfg.entry());

        // The adapter only borrows `doms`; that borrow ends before `doms` moves
        // into `Self`.
        let (frontiers, preorder) = {
            let adapter = CfgDomTree { cfg, doms: &doms };
            (
                graph_algorithms::dominance::dominance_frontiers(&adapter, cfg.entry()),
                graph_algorithms::dominance::dominator_tree_preorder(&adapter, cfg.entry()),
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
    pub(crate) fn iterated_frontier<D>(
        &self,
        def_sites: &D,
    ) -> FxHashMap<RegionId, FxHashSet<D::Var>>
    where
        D: DefSites<Node = RegionId>,
    {
        graph_algorithms::dominance::phi_placement(&self.frontiers, def_sites)
    }
}
