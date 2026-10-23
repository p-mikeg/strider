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

/// The ISA-mode bit flowing into `addr`, falling back to `entry_bit`.
///
/// Only meaningful while the region owning `addr` is decoding, and
/// `Builder::sample_flowing_isa_bit` is the one place that reads it there.
/// Sleigh context flows forward and both re-imposers write only on drift, so a
/// region explored later and starting below `addr` paints its own mode over it;
/// a read after the build answers for that later region.
/// [`Cfg::flowing_isa_bit_at_site`] carries the seal-time answer out instead.
pub(crate) fn flowing_isa_bit_at<R: rsleigh::MemReader>(
    arch: &strider_target::SleighArch,
    sleigh: &rsleigh::Sleigh<R>,
    addr: u64,
    entry_bit: bool,
) -> bool {
    arch.isa_mode_var().map_or(entry_bit, |var| {
        sleigh
            .get_context_at(addr, var)
            .map_or(entry_bit, |mode| mode != 0)
    })
}

pub use engine::DecodeEngine;
pub use incomplete::{Incompleteness, Reason};
pub use indirect_resolver::{Arms, ResolvedTarget, ResolvedTargets};
pub use options::{CfgOptions, DataRanges, FunctionBound, NO_TARGETS, seated_from, seated_in};
pub use pcode_text::{insn_plain_text, insn_text};

pub(crate) use query::is_addr_tail_call;
pub use query::{IfRegionSuccessors, direct_call_target, user_op_of};
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
    pub(crate) undecodable_seeded: Vec<UndecodableTarget>,
    pub(crate) incomplete: Incompleteness,
    pub(crate) link_register_seated: Vec<types::PcodeInsnAddr>,
    pub(crate) tail_call_seated: Vec<types::PcodeInsnAddr>,
    pub(crate) flowing_isa_bits: std::collections::BTreeMap<types::PcodeInsnAddr, bool>,
    pub(crate) space_ids: rsleigh::SpaceIds,
}

impl Cfg {
    pub fn region_graph(&self) -> &RegionGraph {
        &self.region_graph
    }

    pub fn entry(&self) -> NodeIndex {
        self.entry
    }

    /// The space ids of the engine that decoded this CFG, which every LOAD /
    /// STORE in it carries; see [`insn_text`].
    pub fn space_ids(&self) -> &rsleigh::SpaceIds {
        &self.space_ids
    }

    /// The region owning `addr`: of the regions containing it, the one with
    /// the greatest start, the rule the build routes every edge by.
    #[must_use]
    pub fn region_containing(&self, addr: PcodeInsnAddr) -> Option<RegionId> {
        self.index
            .containing(&self.region_graph, addr)
            .map(|(id, _)| id)
    }

    /// Caller-seeded or classifier-derived targets that failed to decode, or
    /// led to code that did, so their edge is absent from this CFG. A
    /// misclassified jump-table bound reaches past the table and yields
    /// addresses that are not code; dropping those keeps the rest of the
    /// function analysable, and reporting them is what stops the CFG being
    /// silently incomplete.
    ///
    /// "Led to" stops at the next dispatch: an arm that reaches the bad bytes
    /// only back through another `Switch` (an outer table's arm into an inner
    /// one, an arm looping to its own header) says nothing about its own bound.
    ///
    /// The ARM is what each entry names, and an arm that led to the bytes
    /// decodes fine; the bytes are reported as [`Reason::UndecodableTarget`].
    ///
    /// Each carries the site it was seated from: the same address reached from
    /// another site decodes in that site's own committed context, so the
    /// failure is no verdict there.
    pub fn undecodable_seeded_targets(&self) -> &[UndecodableTarget] {
        &self.undecodable_seeded
    }

    /// What this build cannot vouch for, each address with its [`Reason`].
    ///
    /// A build resolves nothing, so its site reasons are every dispatch it
    /// left without an edge and every answer it seated. The CFG
    /// `Strider::analyze` returns carries that analysis's set instead.
    pub fn incomplete(&self) -> &Incompleteness {
        &self.incomplete
    }

    /// This CFG reporting `incomplete` in place of its own build's: how
    /// `Strider::analyze`, over several builds, reports through the one it
    /// returns. For that caller only.
    #[doc(hidden)]
    #[must_use]
    pub fn with_incompleteness(mut self, incomplete: Incompleteness) -> Self {
        self.incomplete = incomplete;
        self
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

    /// The ISA-mode bit that flowed into the indirect branch at `site`, sampled
    /// while that site's region was decoded: the mode a target seated there
    /// with no mode of its own decodes in.
    ///
    /// `None` for an address no indirect branch was sealed at, and on an arch
    /// with no ISA-mode var. Where the branch address carries no readable
    /// context, the sample is the function's own mode.
    pub fn flowing_isa_bit_at_site(&self, site: types::PcodeInsnAddr) -> Option<bool> {
        self.flowing_isa_bits.get(&site).copied()
    }
}

pub type RegionId = NodeIndex;
