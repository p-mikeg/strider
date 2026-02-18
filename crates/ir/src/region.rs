use cranelift_entity::{SecondaryMap, entity_impl};
use crate::node::{NodeId, NodeOutputId};
use crate::builder::FunctionBuilder;
use crate::builder::VarId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionId(u32);
entity_impl!(RegionId);


#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Region {
    terminated: bool,
    // The control node that represents the start of the region
    control_node: NodeId,
    // The memory node that represents the start of the region
    memory_node: NodeId,
    // Current control state in the region
    cur_ctrl: NodeOutputId,
    // Current memory state in the region
    cur_memory: NodeOutputId,
    // Current state of the variables in the region
    variables: SecondaryMap<VarId, NodeOutputId>,
    // Initial state of the variables in the region
    initial_variables: SecondaryMap<VarId, NodeOutputId>,
}

pub(crate) struct TerminatedRegion {
    pub(crate) control: NodeOutputId,
    pub(crate) memory: NodeOutputId,
    pub(crate) region_id: RegionId,
}

impl FunctionBuilder {
    pub(crate) fn require_cur_region(&self) -> RegionId {
        let region_id = self.cur_region.expect("current region not set");
        assert!(
            !self.regions[region_id].terminated,
            "attempted to insert into terminated region {}",
            region_id.as_u32()
        );
        region_id
    }

    pub(crate) fn cur_region_control(&self) -> NodeOutputId {
        self.regions[self.require_cur_region()].cur_ctrl
    }

    pub(crate) fn cur_region_memory(&self) -> NodeOutputId {
       self.regions[self.require_cur_region()].cur_memory
    }

    pub(crate) fn advance_cur_region_ctrl(&mut self, ctrl: NodeOutputId) {
        assert!(self.graph().output_kind(ctrl).is_control());
        let region_id = self.require_cur_region();
        self.regions[region_id].cur_ctrl = ctrl;
    }

    pub(crate) fn advance_cur_region_memory(&mut self, memory: NodeOutputId) {
        assert!(self.graph().output_kind(memory).is_memory());
        let region_id = self.require_cur_region();
        self.regions[region_id].cur_memory = memory;
    }

    pub(crate) fn terminate_cur_region(&mut self) -> TerminatedRegion{
        let region_id = self.require_cur_region();
        let control = self.regions[region_id].cur_ctrl;
        let memory = self.regions[region_id].cur_memory;
        self.regions[region_id].terminated = true;
        TerminatedRegion {
            control, memory, region_id
        }
    }

    #[inline]
    pub fn set_region(&mut self, region: RegionId) {
        self.cur_region = Some(region);
    }

    pub(crate) fn link_region_variables(&mut self, region: RegionId, variables: &SecondaryMap<VarId, NodeOutputId>) {
        // Add a dependency between the the parent variable and the current region corresponding variable
        for var_id in variables.keys(){
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
            let current_variable = variables[var_id];
            self.graph_mut().add_node_input(region_variable_id, current_variable);
        }
    }

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

    pub fn write_variable_from_id(&mut self, var_id: VarId, value: NodeOutputId) {
        let region_id = self.require_cur_region();
        self.regions[region_id].variables[var_id] = value;
    }

    pub(crate) fn read_variable_from_id(&self, var_id: VarId) -> NodeOutputId {
        let region_id = self.require_cur_region();
        self.regions[region_id].variables[var_id]
    }

    pub(crate) fn link_control_regions(&mut self, region: RegionId, control: NodeOutputId) {
        assert!(self.graph().output_kind(control).is_control());
        let control_node = self.regions[region].control_node;
        self.graph_mut().add_node_input(control_node, control);
    }

    pub(crate) fn link_memory_regions(&mut self, region: RegionId, memory: NodeOutputId) {
        assert!(self.graph().output_kind(memory).is_memory());
        let memory_node = self.regions[region].memory_node;
        self.graph_mut().add_node_input(memory_node, memory);
    }

    pub(crate) fn link_region(&mut self, region: RegionId, control: NodeOutputId, memory: NodeOutputId, cur_region: RegionId) {
        self.link_control_regions(region, control);
        self.link_memory_regions(region, memory);

        // Add a dependency between the the parent variable and the current region corresponding variable
        for var_id in self.regions[region].variables.keys(){
            let region_variable_output_id = self.regions[region].initial_variables[var_id];
            let region_variable_id = self.graph().get_node_from_output(region_variable_output_id);
            let current_variable = self.regions[cur_region].variables[var_id];
            self.graph_mut().add_node_input(region_variable_id, current_variable);
        }
    }

    pub fn link_regions(&mut self, parent_region: RegionId, child_region: RegionId) {
        self.link_region(
            child_region, 
            self.regions[parent_region].cur_ctrl, 
            self.regions[parent_region].cur_memory, 
            parent_region
        );
    }

}