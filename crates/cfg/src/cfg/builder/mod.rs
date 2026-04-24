mod region_builder;
mod split;

#[doc(hidden)]
pub use region_builder::test_api as region_builder_test_api;

use region_builder::RegionBuilder;

use std::collections::{BTreeMap, VecDeque};

use petgraph::graph::NodeIndex;

use crate::cfg::options::Options;
use crate::cfg::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionGraph};
use crate::cfg::Cfg;
use crate::error::{ErrorKind, Result};

/// Incrementally constructs a [`Cfg`] from a binary entry point.
///
/// The builder uses a work-queue that is seeded with the entry address.
/// Items are popped one at a time; each item triggers decoding of a new
/// region (via [`RegionBuilder`]) or routing of an edge to an existing
/// region.  When a branch target lands in the middle of an already-decoded
/// region, that region is split in two.
///
/// # Usage
/// ```rust,ignore
/// let cfg = Builder::new(sleigh, fn_addr, opts).build()?;
/// ```
pub struct Builder<R: rsleigh::MemReader> {
    pub(super) sleigh: rsleigh::Sleigh<R>,
    /// Virtual address at which the function entry point begins.
    pub(super) start_addr: MachineInsnAddr,
    pub(super) options: Options,
    /// The graph being constructed.
    pub(super) graph: RegionGraph,
    /// Maps each region's `start_addr` to its [`NodeIndex`].
    /// Used by `find_region_containing_addr` and `split_region`.
    pub(super) start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>,
    /// Pending addresses to explore, together with the parent edge they
    /// should connect from.  Processed LIFO (depth-first).
    pub(super) work_queue: VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)>,
}

impl<R: rsleigh::MemReader> Builder<R> {
    /// Creates a new `Builder` that will construct a CFG starting at
    /// `start_addr` using `sleigh` to disassemble instructions.
    pub fn new(sleigh: rsleigh::Sleigh<R>, start_addr: u64, options: Options) -> Self {
        Self {
            sleigh,
            start_addr: start_addr.into(),
            options,
            graph: RegionGraph::new(),
            start_addr_to_region_id: BTreeMap::new(),
            work_queue: VecDeque::new(),
        }
    }

    /// Inserts `region` into the graph and records its start address in the
    /// lookup map.  Returns the assigned [`NodeIndex`].
    ///
    /// # Errors
    /// Returns [`ErrorKind::EmptyRegion`] if `region.insns` is empty.
    pub(super) fn add_region(&mut self, region: Region) -> Result<NodeIndex> {
        if region.insns.is_empty() {
            return Err(ErrorKind::EmptyRegion(region).into());
        }

        let start_addr = region.start_addr;
        let region_id = self.graph.add_node(region);
        self.start_addr_to_region_id.insert(start_addr, region_id);
        Ok(region_id)
    }

    /// Finds the region that contains `addr`, if any.
    ///
    /// Uses a BTreeMap range query to find the last region whose
    /// `start_addr <= addr`, then confirms that `addr` also falls within the
    /// region's instruction range via [`Region::contains_addr`].
    pub(super) fn find_region_containing_addr(&self, addr: PcodeInsnAddr) -> Option<(NodeIndex, &Region)> {
        // Find the last region whose start_addr <= addr
        let (_, &region_id) = self.start_addr_to_region_id.range(..=addr).next_back()?;

        let region = self.graph.node_weight(region_id)?;
        if region.contains_addr(addr) {
            Some((region_id, region))
        } else {
            None
        }
    }

    /// Returns the pcode address corresponding to the function entry point.
    #[inline]
    fn start_pcode_addr(&self) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: self.start_addr,
            insn_index: 0,
        }
    }

    /// Routes `addr` to either an existing region or a new one.
    ///
    /// - If a region already contains `addr` at its *start*, just adds the
    ///   incoming edge.
    /// - If a region contains `addr` in its *interior*, calls [`split_region`](Self::split_region)
    ///   to split it and then adds the edge to the second half.
    /// - If no region contains `addr`, calls [`explore_new_region`](Self::explore_new_region).
    fn explore(
        &mut self,
        parent_region: Option<(NodeIndex, RegionEdgeKind)>,
        addr: PcodeInsnAddr,
    ) -> Result<()> {
        let existing_region = self.find_region_containing_addr(addr);
        if let Some((region_id, region)) = existing_region {
            // This is the case that someone just referenced our region - add an edge between them
            let (parent_region_id, edge_kind) =
                parent_region.ok_or(ErrorKind::MissingParentEdge)?;
            // We checked and the address is within the current region and needs to start a new region
            // This means we reached here by jumping to the middle of a region and the current region needs to be split in 2
            if region.start_addr != addr {
                // found a jump to the middle of the region. we need to split it.
                let second_region = self.split_region(region_id, addr)?;
                self.graph
                    .add_edge(parent_region_id, second_region, edge_kind);
            } else {
                self.graph.add_edge(parent_region_id, region_id, edge_kind);
            }
        } else {
            // This is not an explored region - explore it
            self.explore_new_region(addr, parent_region)?;
        }
        Ok(())
    }

    /// Creates a [`RegionBuilder`] anchored at `start_addr` and decodes
    /// instructions until the region is complete.
    fn explore_new_region(
        &mut self,
        start_addr: PcodeInsnAddr,
        parent_edge: Option<(NodeIndex, RegionEdgeKind)>,
    ) -> Result<()> {
        RegionBuilder::new(self, start_addr, parent_edge).build()?;
        Ok(())
    }

    /// Builds and returns the completed [`Cfg`].
    ///
    /// Seeds the work queue with the entry address, processes items until the
    /// queue is empty, then locates the entry region.
    pub fn build(mut self) -> Result<Cfg<R>> {
        self.work_queue.push_back((None, self.start_pcode_addr()));
        while let Some((parent_region, address)) = self.work_queue.pop_back() {
            self.explore(parent_region, address)?;
        }
        let (starting_region, _) = self
            .find_region_containing_addr(self.start_pcode_addr())
            .ok_or(ErrorKind::FailedCreatingStartRegion)?;

        Ok(Cfg {
            graph: self.graph,
            sleigh: self.sleigh,
            entry: starting_region,
        })
    }
}

#[doc(hidden)]
pub mod test_api {
    //! Test-only forwarders for `Builder` internals.

    use super::Builder;
    use crate::cfg::types::{RegionEdgeKind, RegionGraph};
    use crate::error::Result;
    use petgraph::graph::NodeIndex;
    use std::collections::{BTreeMap, VecDeque};

    pub use crate::cfg::options::Options;
    pub use crate::cfg::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction};

    pub fn add_region<R: rsleigh::MemReader>(
        b: &mut Builder<R>,
        region: Region,
    ) -> Result<NodeIndex> {
        b.add_region(region)
    }

    #[must_use]
    pub fn find_region_containing_addr<R: rsleigh::MemReader>(
        b: &Builder<R>,
        addr: PcodeInsnAddr,
    ) -> Option<(NodeIndex, &Region)> {
        b.find_region_containing_addr(addr)
    }

    pub fn split_region<R: rsleigh::MemReader>(
        b: &mut Builder<R>,
        region_id: NodeIndex,
        addr: PcodeInsnAddr,
    ) -> Result<NodeIndex> {
        b.split_region(region_id, addr)
    }

    #[must_use]
    pub fn graph<R: rsleigh::MemReader>(b: &Builder<R>) -> &RegionGraph {
        &b.graph
    }

    pub fn graph_mut<R: rsleigh::MemReader>(b: &mut Builder<R>) -> &mut RegionGraph {
        &mut b.graph
    }

    #[must_use]
    pub fn start_addr_to_region_id<R: rsleigh::MemReader>(
        b: &Builder<R>,
    ) -> &BTreeMap<PcodeInsnAddr, NodeIndex> {
        &b.start_addr_to_region_id
    }

    #[must_use]
    pub fn work_queue<R: rsleigh::MemReader>(
        b: &Builder<R>,
    ) -> &VecDeque<(Option<(NodeIndex, RegionEdgeKind)>, PcodeInsnAddr)> {
        &b.work_queue
    }

    #[must_use]
    pub fn sleigh<R: rsleigh::MemReader>(b: &Builder<R>) -> &rsleigh::Sleigh<R> {
        &b.sleigh
    }
}
