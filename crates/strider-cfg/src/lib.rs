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
//!
//! IR-free by design: `strider-lift` lifts a finished [`Cfg`] into the
//! `strider_ir` sea-of-nodes, so this crate stays a leaf with no analysis
//! dependency.
//!
//! [`RegionTerminator`] is the single source of truth for how a region
//! transfers control; CFG edges carry no weight.

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
    /// Read-only by convention: mutating this directly desyncs
    /// `start_addr_to_region_id` and silently corrupts later
    /// `region_id_at_start` lookups.  A past bug did exactly that.
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
