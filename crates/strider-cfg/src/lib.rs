mod builder;
mod dot;
mod engine;
mod incomplete;
mod indirect_resolver;
mod neighborhood;
mod options;
mod pcode_text;
mod query;
mod region_index;
#[cfg(test)]
mod test_support;
mod types;

pub type Result<T> = anyhow::Result<T>;

pub use engine::DecodeEngine;
pub use incomplete::{Incompleteness, Reason};
pub use indirect_resolver::{Arms, ResolvedTarget, ResolvedTargets};
pub use options::{
    CfgOptions, DataRanges, FunctionBound, LocalEntries, NO_LOCAL_ENTRIES, NO_TARGETS, seated_from,
    seated_in,
};
pub use pcode_text::{insn_plain_text, insn_text};

pub use query::{IfRegionSuccessors, direct_call_target, user_op_of};
pub(crate) use query::{is_addr_tail_call, is_fall_through_past_end};
pub use types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator, UndecodableTarget,
};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed CFG for a single function, produced by [`DecodeEngine::build_cfg`].
#[derive(Debug)]
pub struct Cfg {
    pub(crate) region_graph: RegionGraph,
    pub(crate) entry: NodeIndex,
    pub(crate) index: region_index::RegionIndex,
    pub(crate) report: BuildReport,
    pub(crate) space_ids: rsleigh::SpaceIds,
}

/// What a build reports beside its graph.
#[derive(Debug, Default)]
pub(crate) struct BuildReport {
    pub(crate) incomplete: Incompleteness,
    pub(crate) undecodable_seeded: Vec<UndecodableTarget>,
    pub(crate) link_register_seated: Vec<PcodeInsnAddr>,
    pub(crate) tail_call_seated: Vec<PcodeInsnAddr>,
    pub(crate) flowing_isa_bits: std::collections::BTreeMap<PcodeInsnAddr, bool>,
}

impl Cfg {
    pub fn region_graph(&self) -> &RegionGraph {
        &self.region_graph
    }

    pub fn entry(&self) -> NodeIndex {
        self.entry
    }

    /// The space ids of the engine that decoded this CFG.
    pub fn space_ids(&self) -> &rsleigh::SpaceIds {
        &self.space_ids
    }

    /// The region owning `addr`: of those containing it, the greatest start.
    #[must_use]
    pub fn region_containing(&self, addr: PcodeInsnAddr) -> Option<RegionId> {
        self.index
            .containing(&self.region_graph, addr)
            .map(|(id, _)| id)
    }

    /// Seeded or derived arms dropped because they, or code they reach before
    /// another `Switch`, failed to decode, each with its site. The failing
    /// bytes are in [`Self::incomplete`].
    pub fn undecodable_seeded_targets(&self) -> &[UndecodableTarget] {
        &self.report.undecodable_seeded
    }

    /// What this build cannot vouch for, each address with its [`Reason`].
    pub fn incomplete(&self) -> &Incompleteness {
        &self.report.incomplete
    }

    /// This CFG reporting `incomplete` in place of its own build's.
    #[doc(hidden)]
    #[must_use]
    pub fn with_incompleteness(mut self, incomplete: Incompleteness) -> Self {
        self.report.incomplete = incomplete;
        self
    }

    /// Live sites seated as a `Return` because the answer was `LinkRegister`.
    pub fn link_register_seated(&self) -> &[PcodeInsnAddr] {
        &self.report.link_register_seated
    }

    /// Live sites seated as a `TailCall` from a single out-of-function target.
    pub fn tail_call_seated(&self) -> &[PcodeInsnAddr] {
        &self.report.tail_call_seated
    }

    /// The ISA-mode bit flowing into the indirect branch at `site`, which a
    /// mode-less target seated there decodes in. `None` off a sealed indirect
    /// branch, or with no ISA-mode var.
    pub fn flowing_isa_bit_at_site(&self, site: PcodeInsnAddr) -> Option<bool> {
        self.report.flowing_isa_bits.get(&site).copied()
    }
}

pub type RegionId = NodeIndex;
