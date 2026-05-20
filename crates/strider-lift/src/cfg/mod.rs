//! Control-flow graph construction for the Strider binary analysis framework.
//!
//! This module lifts a binary function to a [`Cfg`] of basic blocks using
//! GHIDRA's Sleigh p-code lifter ([`rsleigh`]).  Each basic block (region) in
//! the CFG contains a sequence of p-code instructions ([`rsleigh::Insn`]).
//!
//! Absorbed from the standalone `cfg` crate (v2 Phase 2 Task 2.3); the
//! original `cfg` crate is now a one-line shim re-exporting this module.
//!
//! # Key types
//!
//! - [`Cfg`] — a control-flow graph parameterized over an arbitrary memory
//!   reader; built via [`Builder`]
//! - [`Builder`] / [`OptionsBuilder`] — fluent constructors for a [`Cfg`]
//! - [`RegionId`] — identifies a basic block within the CFG
//! - [`RegionEdgeKind`] — `Fallthrough`, `Branch`, `IfCaseTrue`, `IfCaseFalse`
//! - [`IfRegionState`] — tracks the resolved/unresolved state of an if-case

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
pub use builder::{IndirectTargetResolver, ResolvedTargets};
pub use decode_cache::DecodeCache;
#[doc(hidden)]
pub use builder::test_api;
pub use options::{FunctionBoundary, OptionsBuilder};

#[doc(hidden)]
pub use dot::test_api as dot_test_api;

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
    /// Kept `pub` so the strider orchestrator can field-move it out of
    /// the consumed `Cfg` between iterations of the indirect-branch
    /// fixed-point loop (see `tests/sleigh_reuse.rs` and the
    /// `into_sleigh()` accessor below).  Mutating it post-build would
    /// be surprising but is not a documented invariant the way the
    /// `graph` / `start_addr_to_region_id` consistency is.
    pub sleigh: rsleigh::Sleigh<R>,
    /// The underlying directed graph.  Nodes are regions; edges are labeled
    /// with [`RegionEdgeKind`].
    ///
    /// **Read-only by convention.**  Direct mutation
    /// (`cfg.graph.remove_node(...)`) would desync
    /// `start_addr_to_region_id` from the petgraph and silently
    /// corrupt subsequent `region_id_at_start` lookups — a prior bug
    /// where direct map mutation produced exactly this divergence
    /// motivated the `pub(crate)` tightening on the index.  New code
    /// should read via
    /// [`Self::graph`].  Field kept `pub` because the
    /// orchestrator's `sleigh_reuse.rs` test pattern partial-moves
    /// `sleigh` out and continues to read `graph` afterward; a
    /// `pub(crate)` tightening with a `graph(&self)` accessor would
    /// fail to borrow `&self` after the partial move.
    pub graph: RegionGraph,
    /// The [`NodeIndex`] of the function entry-point region.
    /// Read-only by convention; same partial-move rationale as
    /// [`Self::graph`].
    pub entry: NodeIndex,
    /// Index from a region's start address to its [`NodeIndex`], for
    /// O(log R) `region_id_at_start` lookups instead of an O(R) graph
    /// scan.  Promoted from `super::builder::Builder`'s field of the
    /// same name at construction.  Maintained by the indirect-branch
    /// resolver when it splices new regions in via `add_region`.
    ///
    /// Tightened to `pub(crate)`.  External readers go through
    /// [`Self::region_id_at_start`]; tests that hand-build a `Cfg`
    /// fixture use [`Self::from_parts_for_tests`] (gated behind
    /// `#[doc(hidden)]`) to construct one without exposing the field
    /// to production mutation paths.  Direct mutation desyncs the
    /// index from `graph`.
    pub(crate) start_addr_to_region_id:
        std::collections::BTreeMap<types::PcodeInsnAddr, NodeIndex>,
}

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Test-only constructor: assemble a `Cfg` from raw parts.
    ///
    /// The `start_addr_to_region_id` field is normally maintained by
    /// [`crate::cfg::Builder::build`] and stays in sync with `graph`.
    /// Hand-built fixtures (used by `cfg/tests/cfg_query.rs` to exercise
    /// `region_branch` / `region_if` on synthetic petgraphs) construct
    /// a `Cfg` directly; this ctor lets them keep working after the
    /// field was tightened to `pub(crate)`, without re-opening the
    /// mutation hazard for production callers.
    #[doc(hidden)]
    #[must_use]
    pub fn from_parts_for_tests(
        sleigh: rsleigh::Sleigh<R>,
        graph: RegionGraph,
        entry: NodeIndex,
        start_addr_to_region_id: std::collections::BTreeMap<types::PcodeInsnAddr, NodeIndex>,
    ) -> Self {
        Self {
            sleigh,
            graph,
            entry,
            start_addr_to_region_id,
        }
    }

    /// Read-only access to the underlying directed graph.
    #[must_use]
    pub fn graph(&self) -> &RegionGraph {
        &self.graph
    }

    /// [`NodeIndex`] of the function entry-point region.
    #[must_use]
    pub fn entry(&self) -> NodeIndex {
        self.entry
    }

    /// Read-only access to the Sleigh handle.
    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        &self.sleigh
    }

    /// Consume the `Cfg` and return the inner Sleigh handle so a
    /// subsequent CFG rebuild can reuse it without re-loading the SLA
    /// spec.  Used by the strider orchestrator between iterations of
    /// the indirect-branch fixed-point loop.
    #[must_use]
    pub fn into_sleigh(self) -> rsleigh::Sleigh<R> {
        self.sleigh
    }
}

/// Type alias for the petgraph [`NodeIndex`] used to identify regions.
pub type RegionId = NodeIndex;

/// `graphwalk::GraphRef` impl for the region graph.  Lets generic
/// traversal helpers (preorder/postorder/reachability/dominance)
/// work on a `Cfg` the same way they work on `strider_ir::Graph`.  Successors
/// are the petgraph out-neighbors of `node` regardless of edge kind
/// (Fallthrough / Branch / IfCaseTrue / IfCaseFalse); callers that
/// need edge-kind filtering should walk `cfg.graph().edges(node)`
/// directly.
impl<R: rsleigh::MemReader> graphwalk::GraphRef for Cfg<R> {
    type NodeId = NodeIndex;

    fn try_successors(
        &self,
        node: NodeIndex,
        mut f: impl FnMut(NodeIndex) -> std::ops::ControlFlow<()>,
    ) -> std::ops::ControlFlow<()> {
        for succ in self.graph.neighbors(node) {
            f(succ)?;
        }
        std::ops::ControlFlow::Continue(())
    }
}
