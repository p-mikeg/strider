use anyhow::anyhow;
use cranelift_entity::{SecondaryMap, entity_impl};

use crate::builder::FunctionBuilder;
use crate::builder::VarId;
use crate::error::Result;
use crate::node::{NodeId, NodeOutputId};

/// A unique identifier for a basic-block region in the IR graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId(u32);
entity_impl!(RegionId);

/// All state associated with a single basic-block region.
///
/// A region owns:
/// - A `ControlState` node (and its output) that acts as the region header.
/// - A `MemPhi` node (and its output) that selects the memory token at the join.
/// - A current variable map (`variables`) that is updated by writes.
/// - An initial variable map (`initial_variables`) recording the
///   `VarPhi` outputs; these receive incoming values as predecessor
///   regions are linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Region {
    /// `true` once a terminator (branch / return) has been emitted.
    terminated: bool,
    /// The `ControlState` node that represents the entry of this region.
    control_node: NodeId,
    /// The `MemPhi` node that selects the memory token for this region.
    memory_node: NodeId,
    /// The current control edge inside this region (advances through calls).
    cur_ctrl: NodeOutputId,
    /// The current memory token inside this region (advances through stores/calls).
    cur_memory: NodeOutputId,
    /// Current SSA value of each variable in this region.
    variables: SecondaryMap<VarId, NodeOutputId>,
    /// `VarPhi` outputs — one per variable — that gather incoming values
    /// from predecessor regions (filled in as predecessors are linked).
    initial_variables: SecondaryMap<VarId, NodeOutputId>,
}

/// The result of terminating the current region: the final control and memory
/// tokens, plus the region id (needed to link successors).
pub(crate) struct TerminatedRegion {
    pub(crate) control: NodeOutputId,
    pub(crate) memory: NodeOutputId,
    pub(crate) region_id: RegionId,
}

impl FunctionBuilder {
    /// Returns `Ok(())` if `output` has `Control` kind; otherwise an error.
    pub(crate) fn require_control_kind(&self, output: NodeOutputId) -> Result<()> {
        let kind = self.graph().output_kind(output);
        if !kind.is_control() {
            return Err(anyhow!(
                "output {output:?} is not a control edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Returns `Ok(())` if `output` has `Memory` kind; otherwise an error.
    pub(crate) fn require_memory_kind(&self, output: NodeOutputId) -> Result<()> {
        let kind = self.graph().output_kind(output);
        if !kind.is_memory() {
            return Err(anyhow!(
                "output {output:?} is not a memory edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Validates that `res.control` and `res.memory` are control/memory
    /// edges respectively — the documented invariant for any
    /// terminator-producing builder method (Return / Branch /
    /// IndirectBranch / CondBranch / CallOther-terminal).  Single
    /// callsite for the (control + memory) pair so a typo can't drop
    /// one of the two checks.
    pub(crate) fn require_terminator_kinds(&self, res: &TerminatedRegion) -> Result<()> {
        self.require_control_kind(res.control)?;
        self.require_memory_kind(res.memory)
    }

    /// Returns the id of the current region, or an error if no region is set
    /// or if the region has already been terminated.
    pub(crate) fn require_cur_region(&self) -> Result<RegionId> {
        let region_id = self.cur_region.ok_or_else(|| {
            anyhow!("no current region is set; call set_region or set_entry_region first")
        })?;
        if self.regions[region_id].terminated {
            let id = region_id.as_u32();
            return Err(anyhow!("attempted to insert into terminated region {id}"));
        }
        Ok(region_id)
    }

    /// Returns the current control-flow edge of the active region.
    pub(crate) fn cur_region_control(&self) -> Result<NodeOutputId> {
        Ok(self.regions[self.require_cur_region()?].cur_ctrl)
    }

    /// Returns the current memory token of the active region.
    pub(crate) fn cur_region_memory(&self) -> Result<NodeOutputId> {
        Ok(self.regions[self.require_cur_region()?].cur_memory)
    }

    /// Advances the control edge of the active region to `ctrl`.
    pub(crate) fn advance_cur_region_ctrl(&mut self, ctrl: NodeOutputId) -> Result<()> {
        self.require_control_kind(ctrl)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_ctrl = ctrl;
        Ok(())
    }

    /// Advances the memory token of the active region to `memory`.
    ///
    /// `pub` (rather than `pub(crate)`) so the strider layer can advance
    /// memory after a `build_call_other_modeled` call whose ABI's
    /// `memory_edge` is true — see
    /// `crates/strider-analyze/src/strider/insn/mod.rs`
    /// `handle_call_other`.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active and
    /// `WrongOutputKind` when `memory` is not a `Memory` edge.
    pub fn advance_cur_region_memory(&mut self, memory: NodeOutputId) -> Result<()> {
        self.require_memory_kind(memory)?;
        let region_id = self.require_cur_region()?;
        self.regions[region_id].cur_memory = memory;
        Ok(())
    }

    /// Marks the active region as terminated and returns its final control
    /// and memory tokens.
    pub(crate) fn terminate_cur_region(&mut self) -> Result<TerminatedRegion> {
        let region_id = self.require_cur_region()?;
        let control = self.regions[region_id].cur_ctrl;
        let memory = self.regions[region_id].cur_memory;
        self.regions[region_id].terminated = true;
        Ok(TerminatedRegion {
            control,
            memory,
            region_id,
        })
    }

    /// Sets the active region to `region`.
    #[inline]
    pub fn set_region(&mut self, region: RegionId) {
        self.cur_region = Some(region);
    }

    /// Adds incoming variable values from `variables` to the `VarPhi`
    /// nodes of `region`.
    pub(crate) fn link_region_variables(
        &mut self,
        region: RegionId,
        variables: &SecondaryMap<VarId, NodeOutputId>,
    ) -> Result<()> {
        for var_id in variables.keys() {
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
            let current_variable = variables[var_id];
            self.graph_mut()
                .add_node_input(region_variable_id, current_variable)?;
        }
        Ok(())
    }

    /// Allocates a new [`Region`] entry and registers it in the region map.
    pub(crate) fn create_region_helper(
        &mut self,
        control_node: NodeId,
        control_id: NodeOutputId,
        memory_node: NodeId,
        memory_id: NodeOutputId,
        initial_variables: SecondaryMap<VarId, NodeOutputId>,
    ) -> Result<RegionId> {
        self.require_memory_kind(memory_id)?;
        self.require_control_kind(control_id)?;
        Ok(self.regions.push(Region {
            terminated: false,
            control_node,
            memory_node,
            cur_ctrl: control_id,
            cur_memory: memory_id,
            variables: initial_variables.clone(),
            initial_variables,
        }))
    }

    /// Writes `value` to variable `var_id` in the active region.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` when no region is active.
    pub fn write_variable_from_id(&mut self, var_id: VarId, value: NodeOutputId) -> Result<()> {
        let region_id = self.require_cur_region()?;
        self.regions[region_id].variables[var_id] = value;
        Ok(())
    }

    /// Reads the current value of variable `var_id` from the active region.
    pub(crate) fn read_variable_from_id(&self, var_id: VarId) -> Result<NodeOutputId> {
        let region_id = self.require_cur_region()?;
        Ok(self.regions[region_id].variables[var_id])
    }

    /// Adds `control` as an incoming control edge to `region`'s `ControlState` node.
    pub(crate) fn link_control_regions(
        &mut self,
        region: RegionId,
        control: NodeOutputId,
    ) -> Result<()> {
        self.require_control_kind(control)?;
        let control_node = self.regions[region].control_node;
        self.graph_mut().add_node_input(control_node, control)
    }

    /// Adds `memory` as an incoming memory edge to `region`'s `MemPhi` node.
    pub(crate) fn link_memory_regions(
        &mut self,
        region: RegionId,
        memory: NodeOutputId,
    ) -> Result<()> {
        self.require_memory_kind(memory)?;
        let memory_node = self.regions[region].memory_node;
        self.graph_mut().add_node_input(memory_node, memory)
    }

    /// Links `region` as a successor of `cur_region`.
    pub(crate) fn link_region(
        &mut self,
        region: RegionId,
        control: NodeOutputId,
        memory: NodeOutputId,
        cur_region: RegionId,
    ) -> Result<()> {
        self.link_control_regions(region, control)?;
        self.link_memory_regions(region, memory)?;
        let source = self.regions[cur_region].variables.clone();
        self.link_region_variables(region, &source)?;
        Ok(())
    }

    /// Links `child_region` as the fallthrough successor of `parent_region`.
    ///
    /// # Errors
    ///
    /// Propagates the variants from `link_region` —
    /// `ExpectedControl` / `ExpectedMemory`
    /// when `parent_region`'s snapshotted edges are mistyped, plus any
    /// `add_node_input` errors when wiring per-variable phi inputs.
    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) -> Result<()> {
        let (ctrl, mem) = (
            self.regions[parent_region].cur_ctrl,
            self.regions[parent_region].cur_memory,
        );
        self.link_region(child_region, ctrl, mem, parent_region)
    }

    /// Returns the entry-boundary `ControlState` `NodeId` of `region`.
    /// Used by the `strider` crate's per-iteration region index to
    /// look up phi-extension targets across orchestrator iterations.
    #[must_use]
    pub fn region_control_node(&self, region: RegionId) -> NodeId {
        self.regions[region].control_node
    }

    /// Returns the entry-boundary `MemPhi` `NodeId` of `region`.
    #[must_use]
    pub fn region_memory_node(&self, region: RegionId) -> NodeId {
        self.regions[region].memory_node
    }

    /// Returns the current control-output of `region` — i.e. the
    /// `Control` `NodeOutputId` consumed by the region's terminator.
    /// At cache-population time this is the region's exit control.
    #[must_use]
    pub fn region_cur_ctrl(&self, region: RegionId) -> NodeOutputId {
        self.regions[region].cur_ctrl
    }

    /// Returns the current memory-output of `region` — the `Memory`
    /// `NodeOutputId` consumed by the region's terminator.
    #[must_use]
    pub fn region_cur_memory(&self, region: RegionId) -> NodeOutputId {
        self.regions[region].cur_memory
    }

    /// Returns the entry-boundary control output (the `Control`
    /// `NodeOutputId` produced by the region's `ControlState`).  Used
    /// by the cache to pin the entry handle across iterations.
    ///
    /// CORRECTNESS: this is the FIRST output (`output_index 0`) of the
    /// `ControlState` node — the `Control` slot.  The second output
    /// (`output_index 1`) is the `PhiToken` consumed
    /// by per-var phis, not the body's control.
    ///
    /// # Errors
    ///
    /// Returns `ExpectedControl` if `region`'s
    /// `ControlState` does not have a Control output at index 0
    /// (graph-construction bug).
    pub fn region_entry_control(&self, region: RegionId) -> Result<NodeOutputId> {
        let cs_id = self.regions[region].control_node;
        self.first_output_matching(cs_id, |k| k.is_control(), "control", region)
    }

    /// Returns the entry-boundary memory output (the `Memory`
    /// `NodeOutputId` produced by the region's `MemPhi`).
    ///
    /// # Errors
    ///
    /// Returns `ExpectedMemory` if `region`'s `MemPhi`
    /// does not have a Memory output (graph-construction bug).
    pub fn region_entry_memory(&self, region: RegionId) -> Result<NodeOutputId> {
        let mp_id = self.regions[region].memory_node;
        self.first_output_matching(mp_id, |k| k.is_memory(), "memory", region)
    }

    /// Returns the first output of `node` whose kind satisfies `pred`.
    /// Shared between [`Self::region_entry_control`] (filter for
    /// `Control` on the ControlState node) and
    /// [`Self::region_entry_memory`] (filter for `Memory` on the
    /// MemPhi node).  `kind_label` and `region` thread through to the
    /// error message when no matching output exists.
    fn first_output_matching(
        &self,
        node: crate::node::NodeId,
        pred: impl Fn(&crate::node::NodeOutputKind) -> bool,
        kind_label: &str,
        region: RegionId,
    ) -> Result<NodeOutputId> {
        let mut first_seen: Option<NodeOutputId> = None;
        for &out in self.graph().node_outputs(node) {
            if first_seen.is_none() {
                first_seen = Some(out);
            }
            let kind = self.graph().output_kind(out);
            if pred(&kind) {
                return Ok(out);
            }
        }
        match first_seen {
            Some(first) => {
                let kind = self.graph().output_kind(first);
                Err(anyhow!(
                    "output {first:?} is not a {kind_label} edge (got {kind:?})"
                ))
            }
            None => Err(anyhow!(
                "region {region:?} node {node:?} has no outputs"
            )),
        }
    }

    /// Returns an iterator over `(VarId, VarPhi NodeOutputId)`
    /// pairs for `region`'s entry-boundary per-var phi nodes.  Used
    /// by the cache to pin the per-var phi `NodeOutputId`s.
    pub fn region_initial_variables(
        &self,
        region: RegionId,
    ) -> impl Iterator<Item = (VarId, NodeOutputId)> + '_ {
        self.regions[region]
            .initial_variables
            .iter()
            .map(|(var_id, &out)| (var_id, out))
    }

    /// Returns an iterator over `(VarId, NodeOutputId)` pairs for
    /// `region`'s exit-boundary variable values — the value of each
    /// tracked variable at the region's terminator.  Used by the
    /// cache to populate `exit_vn_to_value`.
    pub fn region_exit_variables(
        &self,
        region: RegionId,
    ) -> impl Iterator<Item = (VarId, NodeOutputId)> + '_ {
        self.regions[region]
            .variables
            .iter()
            .map(|(var_id, &out)| (var_id, out))
    }
}
