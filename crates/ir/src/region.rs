use crate::builder::FunctionBuilder;
use crate::builder::VarId;
use crate::error::{ErrorKind, Result};
use crate::node::{NodeId, NodeOutputId};
use cranelift_entity::{SecondaryMap, entity_impl};

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
///   `ControlPhi` outputs; these receive incoming values as predecessor
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
    /// `ControlPhi` outputs — one per variable — that gather incoming values
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
    /// Returns `Ok(())` if `output` has `Control` kind; otherwise
    /// `Err(ExpectedControl)`.
    fn require_control_kind(&self, output: NodeOutputId) -> Result<()> {
        let kind = self.graph().output_kind(output);
        if !kind.is_control() {
            return Err(ErrorKind::ExpectedControl(output, kind).into());
        }
        Ok(())
    }

    /// Returns `Ok(())` if `output` has `Memory` kind; otherwise
    /// `Err(ExpectedMemory)`.
    fn require_memory_kind(&self, output: NodeOutputId) -> Result<()> {
        let kind = self.graph().output_kind(output);
        if !kind.is_memory() {
            return Err(ErrorKind::ExpectedMemory(output, kind).into());
        }
        Ok(())
    }

    /// Returns the id of the current region, or an error if no region is set
    /// or if the region has already been terminated.
    pub(crate) fn require_cur_region(&self) -> Result<RegionId> {
        let region_id = self.cur_region.ok_or(ErrorKind::NoCurrentRegion)?;
        if self.regions[region_id].terminated {
            return Err(ErrorKind::RegionTerminated(region_id.as_u32()).into());
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
    pub(crate) fn advance_cur_region_memory(&mut self, memory: NodeOutputId) -> Result<()> {
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

    /// Adds incoming variable values from `variables` to the `ControlPhi`
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
    /// Returns [`crate::ErrorKind::NoCurrentRegion`] when no region is active.
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

        for var_id in self.regions[region].variables.keys() {
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
            let current_variable = self.regions[cur_region].variables[var_id];
            self.graph_mut()
                .add_node_input(region_variable_id, current_variable)?;
        }
        Ok(())
    }

    /// Links `child_region` as the fallthrough successor of `parent_region`.
    ///
    /// # Errors
    ///
    /// Propagates the variants from [`Self::link_region`] —
    /// [`crate::ErrorKind::ExpectedControl`] / [`crate::ErrorKind::ExpectedMemory`]
    /// when `parent_region`'s snapshotted edges are mistyped, plus any
    /// `add_node_input` errors when wiring per-variable phi inputs.
    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) -> Result<()> {
        let (ctrl, mem) = (
            self.regions[parent_region].cur_ctrl,
            self.regions[parent_region].cur_memory,
        );
        self.link_region(child_region, ctrl, mem, parent_region)
    }
}
