#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

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
