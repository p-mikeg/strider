use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::checks::{
    require_bool_value, require_output_type, require_slot, require_value_kind,
    validate_value_inputs,
};
use crate::builder::{IRBuilder, IRBuilderExt};
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::region::RegionId;

impl FunctionBuilder {
    /// [`Self::build_return_node`], dropping the node.
    pub fn build_return(&mut self, value: Option<ValueId>, ret_values: &[ValueId]) -> Result<()> {
        self.build_return_node(value, ret_values).map(drop)
    }

    /// Value slots are `value` when present, then `ret_values` in order.
    /// Terminates the current region; terminating it again is an error.
    pub fn build_return_node(
        &mut self,
        value: Option<ValueId>,
        ret_values: &[ValueId],
    ) -> Result<NodeId> {
        let res = self.terminate_cur_region()?;
        validate_value_inputs(self.function(), value.as_slice())?;
        validate_value_inputs(self.function(), ret_values)?;
        let inputs = [res.control, res.memory].into_iter().chain(value);
        Ok(self.create_node(
            NodeKind::Return,
            inputs.chain(ret_values.iter().copied()),
            [],
        ))
    }

    /// Control sink for a no-return direct `Call`, consuming control only:
    /// the region's memory token is left unanchored. Terminates the current
    /// region. A region that MERGES a memory effect needs a reader below the
    /// join, so sinking a join straight into this is rejected as
    /// `UnmergedMemoryEffect`.
    pub fn build_unreachable(&mut self) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.create_node(NodeKind::Unreachable, [res.control], []);
        Ok(())
    }

    /// Test-only: [`Self::build_indirect_branch_with_mode`] with no mode bit.
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_indirect_branch(&mut self, target_value: ValueId) -> Result<NodeId> {
        self.build_indirect_branch_with_mode(target_value, None)
    }

    /// Anchors `target_value` on an `IndirectBranch` placeholder, carrying the
    /// interworking ISA-mode bit `isa_mode` the branch's instruction commits
    /// (slot 3), so the resolver can decode each resolved target in it. `None`
    /// for a non-switching branch. Terminates the current region.
    pub fn build_indirect_branch_with_mode(
        &mut self,
        target_value: ValueId,
        isa_mode: Option<ValueId>,
    ) -> Result<NodeId> {
        let res = self.terminate_cur_region()?;
        require_value_kind(self.function(), target_value)?;
        validate_value_inputs(self.function(), isa_mode.as_slice())?;
        Ok(self.create_node(
            NodeKind::IndirectBranch,
            [res.control, res.memory, target_value]
                .into_iter()
                .chain(isa_mode),
            [],
        ))
    }

    /// Test-only: terminates the current region with an unconditional branch
    /// to `dest`.
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_branch(&mut self, dest: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;
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

        require_bool_value(self.function(), cond)?;

        let brcond = self.create_node(
            NodeKind::If,
            [res.control, cond],
            [ValueKind::Control, ValueKind::Control],
        );
        let [true_ctrl_id, false_ctrl_id] = self.function().node_outputs_exact(brcond)?;

        self.link_region(true_region, true_ctrl_id, res.memory, res.region_id)?;
        self.link_region(false_region, false_ctrl_id, res.memory, res.region_id)
    }

    /// Test-only: [`Self::build_switch_with_mode`] with no mode bit.
    #[cfg(any(test, feature = "test-util"))]
    pub fn build_switch(&mut self, address: ValueId, arms: &[(RegionId, u64)]) -> Result<NodeId> {
        self.build_switch_with_mode(address, None, arms)
    }

    /// One `Control` output per arm, wired to that arm's region in order, with
    /// the case addresses in a fresh switch table. Output `i` is taken when
    /// `address == arms[i].1`. Requires `arms` non-empty, and terminates the
    /// current region. `isa_mode` is the ISA-mode bit the dispatch instruction
    /// commits (slot 2), so re-deriving the table can decode each arm in its
    /// own mode; `None` for a non-switching dispatch.
    pub fn build_switch_with_mode(
        &mut self,
        address: ValueId,
        isa_mode: Option<ValueId>,
        arms: &[(RegionId, u64)],
    ) -> Result<NodeId> {
        debug_assert!(!arms.is_empty(), "build_switch requires at least one arm");
        let res = self.terminate_cur_region()?;
        require_value_kind(self.function(), address)?;
        validate_value_inputs(self.function(), isa_mode.as_slice())?;

        let targets: Vec<u64> = arms.iter().map(|&(_, a)| a).collect();
        let table = self.function_mut().add_switch_table(targets);
        let sw = self.create_node(
            NodeKind::Switch(table),
            [res.control, address].into_iter().chain(isa_mode),
            std::iter::repeat_n(ValueKind::Control, arms.len()),
        );
        let out_ctrls: Vec<ValueId> = self.function().node_outputs(sw).to_vec();
        for (&(region, _addr), &ctrl) in arms.iter().zip(&out_ctrls) {
            self.link_region(region, ctrl, res.memory, res.region_id)?;
        }
        Ok(sw)
    }

    /// Advances the region's memory token.
    pub fn build_store(
        &mut self,
        addr: ValueId,
        data: ValueId,
        space: rsleigh::VnSpace,
    ) -> Result<()> {
        let memory = self.cur_region_memory()?;
        let kind = NodeKind::Store(space);
        for (slot, value) in [memory, addr, data].into_iter().enumerate() {
            require_slot(self.function(), &kind, slot, value)?;
        }
        let node_id = self.create_node(kind, [memory, addr, data], [ValueKind::Memory]);
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
        let kind = NodeKind::Load(space);
        for (slot, value) in [memory, addr].into_iter().enumerate() {
            require_slot(self.function(), &kind, slot, value)?;
        }
        require_output_type(&kind, output_type)?;
        Ok(self.build_single_output_pure(kind, [memory, addr], output_type))
    }

    /// A `Phi` on the owning `Region`'s `phi_token`, with no operands yet:
    /// each edge into the region appends one.
    pub(crate) fn build_vn_phi(&mut self, var: rsleigh::Vn, phi_token: ValueId) -> Result<ValueId> {
        let output_type = ValueType::int_from_bytes(var.size)?;
        let phi_value = self.build_single_output_pure(NodeKind::Phi, [phi_token], output_type);
        self.function_mut().set_vn_for_value(phi_value, var);
        Ok(phi_value)
    }
}
