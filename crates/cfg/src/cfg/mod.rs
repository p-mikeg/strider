mod builder;
mod dot;
mod options;
mod query;
mod types;

pub use builder::Builder;
pub use builder::ResolvedTargets;
#[doc(hidden)]
pub use builder::test_api;
pub use options::OptionsBuilder;

#[doc(hidden)]
pub use dot::test_api as dot_test_api;

#[doc(hidden)]
pub use builder::indirect_resolve_test_api;
#[doc(hidden)]
pub use builder::region_builder_test_api;
pub use query::IfRegionState;
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
    /// **Persistence contract** (W11 / Sleigh persistence work): the
    /// Sleigh handle is owned by the [`Cfg`] across the analysis
    /// lifetime and threaded through every iteration of the indirect-
    /// branch fixed-point orchestrator.  Each iteration: (1) builds a
    /// new [`Cfg`] via [`Builder`] (consuming the Sleigh by value);
    /// (2) harvests the Sleigh out of [`Cfg::sleigh`] before dropping
    /// the [`Cfg`]; (3) re-uses the same Sleigh in the next iteration
    /// build.  This avoids re-loading the SLA spec on every CFG
    /// rebuild — a measurable hot-path cost the orchestrator's
    /// fixed-point loop pays for every indirect-branch resolution.
    ///
    /// Reusing one Sleigh across many `lift_one` calls is sound:
    /// `lift_one` mutates only Sleigh's internal decode buffers,
    /// which are reset on every call; there is no per-CFG state in
    /// Sleigh.  This is the same pattern `cfg::region_builder`
    /// already uses within a single CFG build; doing it across
    /// iterations gives the same property at the orchestrator scale.
    ///
    /// The field is also retained so register names can be resolved
    /// for visualisation (the historical reason it was kept).
    pub sleigh: rsleigh::Sleigh<R>,
    /// The underlying directed graph.  Nodes are regions; edges are labeled
    /// with [`RegionEdgeKind`].
    pub graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    pub entry: NodeIndex,
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;
