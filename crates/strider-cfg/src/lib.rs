mod builder;
mod dot;
mod indirect_resolver;
mod neighborhood;
mod options;
mod query;
#[cfg(test)]
mod test_support;
mod types;

pub type Result<T> = anyhow::Result<T>;

pub use builder::{Builder, FlowContext, FlowVars};
pub use indirect_resolver::{ResolvedTarget, ResolvedTargets};
pub use options::CfgOptions;

pub use query::IfRegionSuccessors;
pub(crate) use query::is_addr_tail_call;
pub use types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed CFG for a single function, produced by [`Builder::build`].
#[derive(Debug)]
pub struct Cfg {
    pub(crate) region_graph: RegionGraph,
    pub(crate) entry: NodeIndex,
    pub(crate) undecodable_seeded: Vec<types::PcodeInsnAddr>,
    pub(crate) isa_mode_conflicts: Vec<types::PcodeInsnAddr>,
    pub(crate) interior_branch_targets: Vec<types::PcodeInsnAddr>,
    pub(crate) link_register_seated: Vec<types::PcodeInsnAddr>,
    pub(crate) tail_call_seated: Vec<types::PcodeInsnAddr>,
    pub(crate) function_isa_bit: Option<bool>,
}

impl Cfg {
    pub fn region_graph(&self) -> &RegionGraph {
        &self.region_graph
    }

    pub fn entry(&self) -> NodeIndex {
        self.entry
    }

    /// Caller-seeded or classifier-derived targets that failed to decode, so
    /// their edge is absent from this CFG. A misclassified jump-table bound
    /// reaches past the table and yields addresses that are not code; dropping
    /// those keeps the rest of the function analysable, and reporting them is
    /// what stops the CFG being silently incomplete.
    pub fn undecodable_seeded_targets(&self) -> &[types::PcodeInsnAddr] {
        &self.undecodable_seeded
    }

    /// Addresses two edges reached carrying different ISA modes. One region
    /// owns the bytes, so the losing edge's path decodes in the other's mode.
    /// Which edge wins is work-queue order, so a caller that cares must treat
    /// these as unresolved rather than trusting either decode.
    pub fn isa_mode_conflicts(&self) -> &[types::PcodeInsnAddr] {
        &self.isa_mode_conflicts
    }

    /// Branch targets interior to a region but off every instruction boundary,
    /// which no split can express.
    ///
    /// The edge was wired to the region that OWNS those bytes, whose stream
    /// starts earlier, so for a DIRECT branch into overlapping code the arm is
    /// not the instruction stream the branch jumps to. An over-approximated
    /// jump-table entry lands here too, where dropping it is right. Either way
    /// the edge is not exact.
    pub fn interior_branch_targets(&self) -> &[types::PcodeInsnAddr] {
        &self.interior_branch_targets
    }

    /// Sites seated as a `Return` because the answer was `LinkRegister`.
    ///
    /// Seating one consumes the site at CFG-build time: no placeholder and no
    /// `Switch` anchor survive, so nothing later can tell that a caller's seed
    /// replaced whatever the classifier would have derived there.
    pub fn link_register_seated(&self) -> &[types::PcodeInsnAddr] {
        &self.link_register_seated
    }

    /// Sites seated as a `TailCall` because the answer was a single target
    /// outside the function.
    ///
    /// Seating one consumes the site at CFG-build time, exactly as
    /// [`Self::link_register_seated`] does: a dispatch that really had more
    /// arms leaves no placeholder and no `Switch` anchor behind, so nothing
    /// later can tell the difference between a genuine tail call and a
    /// dispatch collapsed to its first derived answer.
    pub fn tail_call_seated(&self) -> &[types::PcodeInsnAddr] {
        &self.tail_call_seated
    }

    /// This function's own ISA mode: the fallback a resolved target decodes in
    /// when the branch address carries no readable context, and the mode for an
    /// arch-wide default. `None` on an arch without an ISA-mode var.
    ///
    /// NOT the live base. `Builder::enqueue_resolved` reads the context AT the
    /// branch address first and reaches for this only when that read fails, so
    /// a caller deciding a seated target's mode has to read the branch
    /// address's context too, or its interworking test can disagree with the
    /// decode.
    pub fn function_isa_bit(&self) -> Option<bool> {
        self.function_isa_bit
    }
}

pub type RegionId = NodeIndex;
