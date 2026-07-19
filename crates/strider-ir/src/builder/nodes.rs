use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Mints the `InitialMemory` node and keeps its `Memory` output as
    /// `entry_memory`.
    pub fn build_entry(&mut self) -> Result<()> {
        let memory_node = self.function_mut().graph_mut().create_node(
            NodeKind::InitialMemory,
            [],
            [ValueKind::Memory],
        );
        let [memory] = self.function().node_outputs_exact(memory_node)?;
        self.entry_memory = memory;
        Ok(())
    }

    /// Value slots are `value` when present, then `ret_values` in order.
    /// Terminates the current region; terminating it again is an error.
    pub fn build_return(&mut self, value: Option<ValueId>, ret_values: &[ValueId]) -> Result<()> {
        let mut ret_inputs: SmallVec<[ValueId; 4]> = SmallVec::new();
        if let Some(v) = value {
            ret_inputs.push(v);
        }
        ret_inputs.extend_from_slice(ret_values);

        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        self.validate_value_inputs(&ret_inputs)?;

        self.create_node(
            NodeKind::Return,
            [res.control, res.memory].into_iter().chain(ret_inputs),
            [],
        );
        Ok(())
    }

    /// Control sink for a no-return direct `Call`. The memory edge is left
    /// dangling: `Unreachable` consumes control only. Terminates the current
    /// region.
    pub fn build_unreachable(&mut self) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        self.create_node(NodeKind::Unreachable, [res.control], []);
        Ok(())
    }

    /// Anchors `target_value` on an `IndirectBranch` placeholder. Terminates
    /// the current region.
    pub fn build_indirect_branch(&mut self, target_value: ValueId) -> Result<NodeId> {
        let res = self.terminate_cur_region()?;

        self.require_terminator_kinds(&res)?;
        self.validate_value_inputs(std::slice::from_ref(&target_value))?;

        let node = self.create_node(
            NodeKind::IndirectBranch,
            [res.control, res.memory, target_value],
            [],
        );
        Ok(node)
    }

    /// Terminates the current region with an unconditional branch to `dest`.
    pub fn build_branch(&mut self, dest: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        self.link_region(dest, res.control, res.memory, res.region_id)
    }

    /// `cond` must be an `I1` value. Terminates the current region.
    pub fn build_if(
        &mut self,
        cond: ValueId,
        true_region: RegionId,
        false_region: RegionId,
    ) -> Result<()> {
        let res = self.terminate_cur_region()?;

        self.require_bool_value(cond)?;
        self.require_control_kind(res.control)?;

        let brcond = self.create_node(
            NodeKind::If,
            [res.control, cond],
            [ValueKind::Control, ValueKind::Control],
        );
        let [true_ctrl_id, false_ctrl_id] = self.function().node_outputs_exact(brcond)?;

        self.link_region(true_region, true_ctrl_id, res.memory, res.region_id)?;
        self.link_region(false_region, false_ctrl_id, res.memory, res.region_id)
    }

    /// One `Control` output per arm, wired to that arm's region in order, with
    /// the case addresses recorded in `switch_targets`. Output `i` is taken
    /// when `address == arms[i].1`. Requires `arms` non-empty, and terminates
    /// the current region.
    pub fn build_switch(&mut self, address: ValueId, arms: &[(RegionId, u64)]) -> Result<()> {
        debug_assert!(!arms.is_empty(), "build_switch requires at least one arm");
        let res = self.terminate_cur_region()?;
        self.require_value_kind(address)?;
        self.require_control_kind(res.control)?;

        let sw = self.create_node(
            NodeKind::Switch,
            [res.control, address],
            std::iter::repeat_n(ValueKind::Control, arms.len()),
        );
        let out_ctrls: Vec<ValueId> = self.function().node_outputs(sw).to_vec();
        for (&(region, _addr), &ctrl) in arms.iter().zip(&out_ctrls) {
            self.link_region(region, ctrl, res.memory, res.region_id)?;
        }
        let targets: Vec<u64> = arms.iter().map(|&(_, a)| a).collect();
        self.function_mut()
            .side_tables_mut()
            .set_switch_targets(sw, targets);
        Ok(())
    }

    /// Advances the region's memory token.
    pub fn build_store(
        &mut self,
        addr: ValueId,
        data: ValueId,
        space: rsleigh::VnSpace,
    ) -> Result<()> {
        let memory = self.cur_region_memory()?;
        self.require_memory_kind(memory)?;
        self.require_value_kind(addr)?;
        self.require_value_kind(data)?;

        let node_id = self.create_node(
            NodeKind::Store(space),
            [memory, addr, data],
            [ValueKind::Memory],
        );
        let [new_mem] = self.function().node_outputs_exact(node_id)?;
        self.advance_cur_region_memory(new_mem)
    }

    pub fn build_load(
        &mut self,
        addr: ValueId,
        space: rsleigh::VnSpace,
        output_type: ValueType,
    ) -> Result<ValueId> {
        let memory = self.cur_region_memory()?;
        self.require_memory_kind(memory)?;
        self.require_value_kind(addr)?;
        Ok(self.build_single_output_pure(NodeKind::Load(space), [memory, addr], output_type))
    }

    /// `phi_token` must be the owning `Region`'s `PhiToken` output.
    /// `incoming_values` holds one value per predecessor, and may be empty at
    /// first: `add_region_predecessor` fills them in later.
    pub(crate) fn build_vn_phi(
        &mut self,
        var: rsleigh::Vn,
        phi_token: ValueId,
        incoming_values: &[ValueId],
    ) -> Result<ValueId> {
        self.require_phi_token_kind(phi_token)?;
        self.validate_value_inputs(incoming_values)?;
        let output_type = ValueType::int_for_byte_size(var.size)?;
        let phi_value = self.build_single_output_pure(
            NodeKind::Phi,
            core::iter::once(phi_token).chain(incoming_values.iter().copied()),
            output_type,
        );
        self.function_mut().set_vn_for_value(phi_value, var);
        Ok(phi_value)
    }
}
