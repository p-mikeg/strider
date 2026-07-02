#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Control-flow graph construction for the Strider binary analysis framework.
//!
//! This crate lifts a binary function to a [`Cfg`] of basic blocks using
//! GHIDRA's Sleigh p-code lifter ([`rsleigh`]).  Each basic block (region) in
//! the CFG contains a sequence of p-code instructions ([`rsleigh::Insn`]).
//! It is IR-free: `strider-lift` lifts a finished [`Cfg`] into the
//! `strider_ir` sea-of-nodes.
//!
//! # Key types
//!
//! - [`Cfg`] — a control-flow graph parameterized over an arbitrary memory
//!   reader; built via [`Builder`]
//! - [`Builder`] — constructs a [`Cfg`] from a [`CfgOptions`]
//! - [`RegionId`] — identifies a basic block within the CFG
//! - [`RegionTerminator`] — how a region ends (the single source of truth for
//!   its control transfer; CFG edges are unweighted topology)
//! - [`IfRegionSuccessors`] — the true / false successor regions of a
//!   conditional-branch region

mod builder;
mod dot;
mod indirect_resolver;
mod options;
mod query;
mod types;

/// Crate-level `Result` alias. Every fallible function in this crate
/// returns this type.
pub type Result<T> = anyhow::Result<T>;

pub use builder::Builder;
pub use indirect_resolver::ResolvedTargets;
pub use options::CfgOptions;

pub use query::{IfRegionSuccessors, is_addr_tail_call};
pub use types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};

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
    start_addr_to_region_id: std::collections::BTreeMap<types::PcodeInsnAddr, NodeIndex>,
}

impl Cfg {
    /// Read-only access to the underlying directed region graph.
    pub fn region_graph(&self) -> &RegionGraph {
        &self.region_graph
    }

    /// [`NodeIndex`] of the function entry-point region.
    pub fn entry(&self) -> NodeIndex {
        self.entry
    }
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;
