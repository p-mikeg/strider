#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Binary function to a [`Cfg`] of basic blocks, via GHIDRA's Sleigh p-code
//! lifter ([`rsleigh`]).  Each region holds a sequence of [`rsleigh::Insn`].

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

pub use builder::Builder;
pub use indirect_resolver::ResolvedTargets;
pub use options::CfgOptions;

pub use query::IfRegionSuccessors;
pub(crate) use query::is_addr_tail_call;
pub use types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};

use types::RegionGraph;

use petgraph::graph::NodeIndex;

/// A completed CFG for a single function, produced by [`Builder::build`].
#[derive(Debug)]
pub struct Cfg {
    /// Must not be mutated directly: `start_addr_to_region_id` would desync.
    pub(crate) region_graph: RegionGraph,
    pub(crate) entry: NodeIndex,
    /// O(log R) start-address index, kept in sync with `region_graph`.
    start_addr_to_region_id: std::collections::BTreeMap<types::PcodeInsnAddr, NodeIndex>,
}

impl Cfg {
    pub fn region_graph(&self) -> &RegionGraph {
        &self.region_graph
    }

    pub fn entry(&self) -> NodeIndex {
        self.entry
    }
}

pub type RegionId = NodeIndex;
