use cranelift_entity::{SecondaryMap, entity_impl};
use crate::node::{NodeId, NodeOutputId};
use crate::builder::FunctionBuilder;
use crate::builder::VarId;

/// A unique identifier for a basic-block region in the IR graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId(u32);
entity_impl!(RegionId);


/// All state associated with a single basic-block region.
///
/// A region owns:
/// - A `ControlState` node (and its output) that acts as the region header.
/// - A `MemSelector` node (and its output) that tracks the memory token.
/// - A current variable map (`variables`) that is updated by writes.
/// - An initial variable map (`initial_variables`) recording the
///   `ControlSelector` phi-node outputs; these receive incoming values as
///   predecessor regions are linked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Region {
    /// `true` once a terminator (branch / return) has been emitted.
    terminated: bool,
    /// The `ControlState` node that represents the entry of this region.
    control_node: NodeId,
    /// The `MemSelector` node that selects the memory token for this region.
    memory_node: NodeId,
    /// The current control edge inside this region (advances through calls).
    cur_ctrl: NodeOutputId,
    /// The current memory token inside this region (advances through stores/calls).
    cur_memory: NodeOutputId,
    /// Current SSA value of each variable in this region.
    variables: SecondaryMap<VarId, NodeOutputId>,
    /// `ControlSelector` phi outputs — one per variable — that gather
    /// incoming values from predecessor regions.
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
    /// Returns the id of the current region, asserting that it exists and has
    /// not been terminated yet.
    ///
    /// Panics if no current region is set or if the region has already been
    /// terminated.
    pub(crate) fn require_cur_region(&self) -> RegionId {
        let region_id = self.cur_region.expect("current region not set");
        assert!(
            !self.regions[region_id].terminated,
            "attempted to insert into terminated region {}",
            region_id.as_u32()
        );
        region_id
    }

    /// Returns the current control-flow edge of the active region.
    pub(crate) fn cur_region_control(&self) -> NodeOutputId {
        self.regions[self.require_cur_region()].cur_ctrl
    }

    /// Returns the current memory token of the active region.
    pub(crate) fn cur_region_memory(&self) -> NodeOutputId {
       self.regions[self.require_cur_region()].cur_memory
    }

    /// Advances the control edge of the active region to `ctrl`.
    ///
    /// Used after `Call` nodes to update the in-flight control token.
    pub(crate) fn advance_cur_region_ctrl(&mut self, ctrl: NodeOutputId) {
        assert!(self.graph().output_kind(ctrl).is_control());
        let region_id = self.require_cur_region();
        self.regions[region_id].cur_ctrl = ctrl;
    }

    /// Advances the memory token of the active region to `memory`.
    ///
    /// Used after `Store` and `Call` nodes.
    pub(crate) fn advance_cur_region_memory(&mut self, memory: NodeOutputId) {
        assert!(self.graph().output_kind(memory).is_memory());
        let region_id = self.require_cur_region();
        self.regions[region_id].cur_memory = memory;
    }

    /// Marks the active region as terminated and returns its final control
    /// and memory tokens.
    ///
    /// After this call the region cannot accept more instructions.
    pub(crate) fn terminate_cur_region(&mut self) -> TerminatedRegion{
        let region_id = self.require_cur_region();
        let control = self.regions[region_id].cur_ctrl;
        let memory = self.regions[region_id].cur_memory;
        self.regions[region_id].terminated = true;
        TerminatedRegion {
            control, memory, region_id
        }
    }

    /// Sets the active region to `region`.
    ///
    /// All subsequent builder calls operate on `region` until it is changed
    /// or terminated.
    #[inline]
    pub fn set_region(&mut self, region: RegionId) {
        self.cur_region = Some(region);
    }

    /// Adds incoming variable values from `variables` to the `ControlSelector`
    /// phi nodes of `region`.
    ///
    /// For each variable in `variables`, the corresponding phi node of
    /// `region` receives `variables[var_id]` as an additional input.
    pub(crate) fn link_region_variables(&mut self, region: RegionId, variables: &SecondaryMap<VarId, NodeOutputId>) {
        // Add a dependency between the parent variable and the current region corresponding variable
        for var_id in variables.keys(){
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
            let current_variable = variables[var_id];
            self.graph_mut().add_node_input(region_variable_id, current_variable);
        }
    }

    /// Allocates a new [`Region`] entry and registers it in the region map.
    ///
    /// The caller is responsible for supplying the pre-created `ControlState`
    /// and `MemSelector` node ids and their output ids.
    pub(crate) fn create_region_helper(
        &mut self,
        control_node: NodeId,
        control_id: NodeOutputId,
        memory_node: NodeId,
        memory_id: NodeOutputId,
        initial_variables: SecondaryMap<VarId, NodeOutputId>
    ) -> RegionId {

        assert!(self.graph().output_kind(memory_id).is_memory());
        assert!(self.graph().output_kind(control_id).is_control());
        self.regions.push(
            Region {
                terminated: false,
                control_node,
                memory_node,
                cur_ctrl: control_id,
                cur_memory: memory_id,
                variables: initial_variables.clone(),
                initial_variables
            }
        )
    }

    /// Writes `value` to variable `var_id` in the active region.
    pub fn write_variable_from_id(&mut self, var_id: VarId, value: NodeOutputId) {
        let region_id = self.require_cur_region();
        self.regions[region_id].variables[var_id] = value;
    }

    /// Reads the current value of variable `var_id` from the active region.
    pub(crate) fn read_variable_from_id(&self, var_id: VarId) -> NodeOutputId {
        let region_id = self.require_cur_region();
        self.regions[region_id].variables[var_id]
    }

    /// Adds `control` as an incoming control edge to `region`'s `ControlState`
    /// node.
    pub(crate) fn link_control_regions(&mut self, region: RegionId, control: NodeOutputId) {
        assert!(self.graph().output_kind(control).is_control());
        let control_node = self.regions[region].control_node;
        self.graph_mut().add_node_input(control_node, control);
    }

    /// Adds `memory` as an incoming memory edge to `region`'s `MemSelector`
    /// node.
    pub(crate) fn link_memory_regions(&mut self, region: RegionId, memory: NodeOutputId) {
        assert!(self.graph().output_kind(memory).is_memory());
        let memory_node = self.regions[region].memory_node;
        self.graph_mut().add_node_input(memory_node, memory);
    }

    /// Links `region` as a successor of `cur_region`: connects the given
    /// control and memory tokens and propagates the current variable state
    /// to `region`'s phi nodes.
    pub(crate) fn link_region(&mut self, region: RegionId, control: NodeOutputId, memory: NodeOutputId, cur_region: RegionId) {
        self.link_control_regions(region, control);
        self.link_memory_regions(region, memory);

        // Add a dependency between the parent variable and the current region corresponding variable
        for var_id in self.regions[region].variables.keys(){
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
            let current_variable = self.regions[cur_region].variables[var_id];
            self.graph_mut().add_node_input(region_variable_id, current_variable);
        }
    }

    /// Links `child_region` as the fallthrough successor of `parent_region`.
    ///
    /// Propagates `parent_region`'s final control, memory, and variable state
    /// to `child_region`.
    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) {
        self.link_region(
            child_region,
            self.regions[parent_region].cur_ctrl,
            self.regions[parent_region].cur_memory,
            parent_region
        );
    }

}
