//! Dominator tree + dominance frontiers over a CFG's region graph — the
//! strider-specific *wiring* for pruned-SSA construction.
//!
//! The CFG itself knows nothing about dominance: it only exposes its region
//! graph ([`Cfg::region_graph`] / [`Cfg::region_ids`] / [`Cfg::entry`]).  This
//! module computes immediate dominators over that graph with petgraph's
//! Cooper–Harvey–Kennedy `simple_fast`, then hands the idom relation to the
//! generic, CFG-agnostic routines in [`graph_algorithms::dominance`] (dominance
//! frontiers, dom-tree preorder, iterated-DF φ placement) via a thin
//! [`DomTree`] adapter.  Nothing graph-theoretic lives here — only the bridge.

use graph_algorithms::{DefSites, DomTree, Frontiers};
use petgraph::Direction::Incoming;
use petgraph::algo::dominators::{Dominators, simple_fast};
use rustc_hash::{FxHashMap, FxHashSet};
use strider_cfg::{Cfg, RegionId};

/// [`DomTree`] adapter over a CFG plus its petgraph-computed idoms — the bridge
/// that lets the generic `graph_algorithms` dominance routines run on a strider
/// CFG through only the CFG's public region-graph surface.
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

/// Dominator tree + dominance frontiers for one CFG, plus a dominator-tree
/// pre-order (the traversal order the SSA renaming walk uses, so a region is
/// visited only after every region that dominates it).
pub(crate) struct DomInfo {
    doms: Dominators<RegionId>,
    /// `frontiers[r]` = the dominance frontier of `r` (the regions where a
    /// definition in `r` first stops dominating — i.e. where a phi may be
    /// needed).  Absent key = empty frontier.
    frontiers: Frontiers<RegionId>,
    /// Dominator-tree pre-order from the entry: every region appears after its
    /// immediate dominator.
    preorder: Vec<RegionId>,
}

impl DomInfo {
    /// Computes the dominator tree, dominance frontiers, and dom-tree pre-order
    /// for `cfg` (all reachable from the entry region).
    #[must_use]
    pub(crate) fn compute(cfg: &Cfg) -> Self {
        let doms = simple_fast(cfg.region_graph(), cfg.entry());

        // Derive the dominance frontiers and dom-tree preorder with the generic
        // routines, driven through the CFG adapter.  The adapter only borrows
        // `doms`; that borrow ends before `doms` is moved into `Self`.
        let (frontiers, preorder) = {
            let adapter = CfgDomTree { cfg, doms: &doms };
            (
                graph_algorithms::dominance_frontiers(&adapter),
                graph_algorithms::dominator_tree_preorder(&adapter, cfg.entry()),
            )
        };

        Self {
            doms,
            frontiers,
            preorder,
        }
    }

    /// The immediate dominator of `r`, or `None` for the entry region and any
    /// region unreachable from the entry.
    #[must_use]
    pub(crate) fn immediate_dominator(&self, r: RegionId) -> Option<RegionId> {
        self.doms.immediate_dominator(r)
    }

    /// Dominator-tree pre-order: every region appears after its immediate
    /// dominator.  Regions unreachable from the entry are excluded.
    #[must_use]
    pub(crate) fn preorder(&self) -> &[RegionId] {
        &self.preorder
    }

    /// Iterated-dominance-frontier φ placement: given a `variable → defining
    /// regions` map, returns the set of variables that need a value `Phi` at each
    /// region (Cytron pruned SSA).  Delegates to [`graph_algorithms::phi_placement`];
    /// `def_sites` is any [`DefSites`] over `RegionId` nodes (e.g. the lifter's
    /// `FxHashMap<InitialVnId, FxHashSet<RegionId>>`).
    #[must_use]
    pub(crate) fn iterated_frontier<D>(&self, def_sites: &D) -> FxHashMap<RegionId, FxHashSet<D::Var>>
    where
        D: DefSites<Node = RegionId>,
    {
        graph_algorithms::phi_placement(&self.frontiers, def_sites)
    }
}
