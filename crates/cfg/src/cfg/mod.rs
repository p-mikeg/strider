mod builder;
mod dot;
mod options;
mod query;
mod types;

pub use builder::Builder;
#[doc(hidden)]
pub use builder::test_api;
pub use options::OptionsBuilder;

#[doc(hidden)]
pub use dot::test_api as dot_test_api;

#[doc(hidden)]
pub use builder::region_builder_test_api;
pub use query::IfRegionState;
pub use types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed Control Flow Graph for a single function.
///
/// Produced by [`Builder::build`].  The graph is a [`petgraph::stable_graph::StableDiGraph`]
/// where each node is a [`Region`] (basic block) and each edge is a
/// [`RegionEdgeKind`] (the type of control transfer).
#[derive(Debug)]
pub struct Cfg<R: rsleigh::MemReader> {
    /// The Sleigh context used during construction.  Retained so that
    /// register names can be resolved for visualisation.
    pub sleigh: rsleigh::Sleigh<R>,
    /// The underlying directed graph.  Nodes are regions; edges are labeled
    /// with [`RegionEdgeKind`].
    pub graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    pub entry: NodeIndex,
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;
