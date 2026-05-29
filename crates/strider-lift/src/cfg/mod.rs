//! Control-flow graph construction for the Strider binary analysis framework.
//!
//! This module lifts a binary function to a [`Cfg`] of basic blocks using
//! GHIDRA's Sleigh p-code lifter ([`rsleigh`]).  Each basic block (region) in
//! the CFG contains a sequence of p-code instructions ([`rsleigh::Insn`]).
//!
//! # Key types
//!
//! - [`Cfg`] — a control-flow graph parameterized over an arbitrary memory
//!   reader; built via [`Builder`]
//! - [`Builder`] / [`OptionsBuilder`] — fluent constructors for a [`Cfg`]
//! - [`RegionId`] — identifies a basic block within the CFG
//! - [`RegionTerminator`] — how a region ends (the single source of truth for
//!   its control transfer; CFG edges are unweighted topology)
//! - [`IfRegionSuccessors`] — the true / false successor regions of a
//!   conditional-branch region

mod builder;
mod decode_cache;
mod dot;
mod options;
mod query;
mod types;

/// Module-level `Result` alias. Every fallible function in `cfg` returns
/// this type.
pub type Result<T> = anyhow::Result<T>;

pub use builder::Builder;
pub use builder::{IndirectResolverFn, ResolvedTargets};
pub use decode_cache::DecodeCache;
pub use options::OptionsBuilder;

pub use query::{IfRegionSuccessors, is_addr_tail_call};
pub use types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator,
};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed Control Flow Graph for a single function.
///
/// Produced by [`Builder::build`].  The graph is a [`petgraph::stable_graph::StableDiGraph`]
/// where each node is a [`Region`] (basic block) and edges are unweighted —
/// the source region's [`RegionTerminator`] classifies the control transfer.
#[derive(Debug)]
pub struct Cfg {
    /// The underlying directed graph.  Nodes are regions; edges are
    /// unweighted topology (`()`), classified by the source
    /// [`RegionTerminator`].
    ///
    /// **Read-only by convention.**  Direct mutation
    /// (`cfg.region_graph.remove_node(...)`) would desync
    /// `start_addr_to_region_id` from the petgraph and silently
    /// corrupt subsequent `region_id_at_start` lookups — a prior bug
    /// where direct map mutation produced exactly this divergence
    /// motivated the `pub(crate)` tightening on the index.  New code
    /// should read via [`Self::region_graph`].
    pub region_graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    pub entry: NodeIndex,
    /// Index from a region's start address to its [`NodeIndex`], for
    /// O(log R) `region_id_at_start` lookups instead of an O(R) graph
    /// scan.  Promoted from `super::builder::Builder`'s field of the
    /// same name at construction.  Maintained by the indirect-branch
    /// resolver when it splices new regions in via `add_region`.
    ///
    /// Tightened to `pub(crate)`.  External readers go through
    /// [`Self::region_id_at_start`].  Direct mutation desyncs the
    /// index from `graph`.
    pub(crate) start_addr_to_region_id:
        std::collections::BTreeMap<types::PcodeInsnAddr, NodeIndex>,
}

impl Cfg {
    /// Read-only access to the underlying directed region graph.
    #[must_use]
    pub fn region_graph(&self) -> &RegionGraph {
        &self.region_graph
    }

    /// [`NodeIndex`] of the function entry-point region.
    #[must_use]
    pub fn entry(&self) -> NodeIndex {
        self.entry
    }
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;

/// `graphwalk::GraphRef` impl for the region graph.  Lets generic
/// traversal helpers (preorder/postorder/reachability/dominance)
/// work on a `Cfg` the same way they work on `strider_ir::Graph`.  Successors
/// are the petgraph out-neighbors of `node` (edges are unweighted topology);
/// callers that need to distinguish how a region exits should read the
/// source region's [`RegionTerminator`] (e.g. via [`Cfg::region_if`]).
impl graphwalk::GraphRef for Cfg {
    type NodeId = NodeIndex;

    fn try_successors(
        &self,
        node: NodeIndex,
        mut f: impl FnMut(NodeIndex) -> std::ops::ControlFlow<()>,
    ) -> std::ops::ControlFlow<()> {
        for succ in self.region_graph.neighbors(node) {
            f(succ)?;
        }
        std::ops::ControlFlow::Continue(())
    }
}
