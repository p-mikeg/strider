use std::collections::HashMap;

use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use super::types::{Region, RegionEdgeKind};
use super::{Cfg, RegionId};
use crate::error::{ErrorKind, Result};

/// The two successors of a conditional-branch region.
///
/// Returned by [`Cfg::region_if`].
pub struct IfRegionState {
    /// Region reached when the branch condition is *true*, if present.
    pub if_true_region: Option<NodeIndex>,
    /// Region reached when the branch condition is *false* (fall-through), if present.
    pub if_false_region: Option<NodeIndex>,
}

impl<R: rsleigh::MemReader> Cfg<R> {
    /// Collects all outgoing edges from `region_id` into a map keyed by edge kind.
    ///
    /// # Errors
    /// Returns [`ErrorKind::DuplicateEdgeKind`] if the same edge kind appears more
    /// than once on a single region (which would indicate a malformed CFG).
    fn following_regions(
        &self,
        region_id: RegionId,
    ) -> Result<HashMap<&RegionEdgeKind, NodeIndex>> {
        let mut next_regions = HashMap::new();
        for edge in self.graph.edges_directed(region_id, petgraph::Outgoing) {
            let kind = edge.weight();
            if next_regions.contains_key(kind) {
                return Err(ErrorKind::DuplicateEdgeKind(region_id, *kind).into());
            }
            next_regions.insert(kind, edge.target());
        }
        Ok(next_regions)
    }

    /// Returns the unconditional-branch successor of `region_id`, if any.
    ///
    /// # Errors
    /// Returns an error if the CFG graph is malformed (duplicate edge kinds).
    pub fn region_branch(&self, region_id: RegionId) -> Result<Option<NodeIndex>> {
        let next_regions = self.following_regions(region_id)?;
        Ok(next_regions.get(&RegionEdgeKind::Branch).copied())
    }

    /// Returns both conditional-branch successors of `region_id`.
    ///
    /// # Errors
    /// Returns an error if the CFG graph is malformed (duplicate edge kinds).
    pub fn region_if(&self, region_id: RegionId) -> Result<IfRegionState> {
        let next_regions = self.following_regions(region_id)?;
        Ok(IfRegionState {
            if_true_region: next_regions.get(&RegionEdgeKind::IfCaseTrue).copied(),
            if_false_region: next_regions.get(&RegionEdgeKind::IfCaseFalse).copied(),
        })
    }

    /// Iterates over all [`Region`]s in the CFG (unordered).
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.graph.node_weights()
    }

    /// Iterates over the [`RegionId`] of every region in the CFG (unordered).
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> {
        self.graph.node_indices()
    }

    /// Returns the pcode instructions contained in `region_id`.
    ///
    /// # Errors
    /// Returns [`ErrorKind::InvalidRegion`] when `region_id` does not exist.
    pub fn region_insn(&self, region_id: NodeIndex) -> Result<Vec<rsleigh::Insn>> {
        let region = self
            .graph
            .node_weight(region_id)
            .ok_or(ErrorKind::InvalidRegion(region_id))?;
        Ok(region
            .insns
            .iter()
            .map(|region_insn| region_insn.insn.clone())
            .collect())
    }
}
