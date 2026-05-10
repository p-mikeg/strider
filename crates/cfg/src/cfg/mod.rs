mod builder;
mod decode_cache;
mod dot;
mod options;
mod query;
mod types;

pub use builder::Builder;
pub use builder::ResolvedTargets;
pub use decode_cache::DecodeCache;
#[doc(hidden)]
pub use builder::test_api;
pub use options::OptionsBuilder;

#[doc(hidden)]
pub use dot::test_api as dot_test_api;

#[doc(hidden)]
pub use builder::indirect_resolve_test_api;
#[doc(hidden)]
pub use builder::region_builder_test_api;
pub use query::{IfRegionState, is_addr_tail_call};
pub use types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction, RegionTerminator,
};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed Control Flow Graph for a single function.
///
/// Produced by [`Builder::build`].  The graph is a [`petgraph::stable_graph::StableDiGraph`]
/// where each node is a [`Region`] (basic block) and each edge is a
/// [`RegionEdgeKind`] (the type of control transfer).
#[derive(Debug)]
pub struct Cfg<R: rsleigh::MemReader> {
    /// The Sleigh context used during construction.
    ///
    /// Owned by the [`Cfg`] across the analysis lifetime; `strider::run`
    /// harvests it out of `Cfg::sleigh` between iterations of the
    /// indirect-branch fixed-point loop and threads it back into the
    /// next [`Builder`] so the SLA spec is loaded once per analysis,
    /// not once per CFG rebuild.  See `tests/sleigh_reuse.rs` for the
    /// round-trip pin.
    ///
    /// Reusing one Sleigh across many `lift_one` calls is sound:
    /// `lift_one` mutates only Sleigh's internal decode buffers,
    /// which are reset on every call; there is no per-CFG state in
    /// Sleigh.
    ///
    /// The field is also retained so register names can be resolved
    /// for visualisation.
    pub sleigh: rsleigh::Sleigh<R>,
    /// The underlying directed graph.  Nodes are regions; edges are labeled
    /// with [`RegionEdgeKind`].
    pub graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    pub entry: NodeIndex,
    /// Index from a region's start address to its [`NodeIndex`], for
    /// O(log R) `region_id_at_start` lookups instead of an O(R) graph
    /// scan.  Promoted from `super::builder::Builder`'s field of the
    /// same name at construction.  Maintained by the indirect-branch
    /// resolver when it splices new regions in via `add_region`.
    ///
    /// kept `pub` (was tightened to `pub(crate)`
    /// then reverted) because `cfg/tests/cfg_query.rs` constructs `Cfg`
    /// via struct-literal syntax for hand-built petgraph fixtures.
    /// External readers should still go through
    /// [`Self::region_id_at_start`] — the field accessor is the
    /// canonical path; direct mutation desyncs the index from `graph`.
    pub start_addr_to_region_id:
        std::collections::BTreeMap<types::PcodeInsnAddr, NodeIndex>,
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;
