use smallvec::SmallVec;

use super::{FunctionBuilder, require_reg_or_unique};
use crate::builder::IRBuilderExt;
use crate::IRViewer;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Resets the graph and emits the function `Entry` and `InitialMemory` nodes.
    ///
    /// # Errors
    ///
    /// Returns `WrongOutputCount` if the freshly created `Entry`
    /// or `InitialMemory` nodes do not have their expected single output
    /// (this would indicate a graph-construction bug, not user error).
    pub fn build_entry(&mut self) -> Result<()> {
        // Reset the function to a fresh empty graph while preserving the
        // calling-convention SSoT (`default_cc` / `all_vns` / `endianness`)
        // that `FunctionBuilder::new` populated.  Resetting in-place keeps
        // the entry/InitialMemory pair as nodes 0/1.
        let default_cc = std::mem::take(&mut self.function.default_cc);
        let all_vns = std::mem::take(&mut self.function.all_vns);
        let endianness = self.function.endianness;
        self.function =
            crate::function::Function::new(default_cc, endianness, all_vns);

        let entry_node = self.create_node(NodeKind::Entry, [], vec![ValueKind::Control]);
        self.function.set_entry(entry_node);

        let memory_node =
            self.create_node(NodeKind::InitialMemory, [], vec![ValueKind::Memory]);
        let [memory] = self.function().node_outputs_exact(memory_node)?;
        self.entry_memory = memory;
        Ok(())
    }

    /// Emits a `Return` node into the current region from the resolved
    /// return-value inputs.
    ///
    /// Terminates the current region with a `Return` node whose value
    /// slots are the explicitly-provided `value` (when `Some`) followed
    /// by the current SSA values of `ret_vars` in order.
    ///
    /// This method **terminates** the current region unconditionally —
    /// callers must not call `mark_cur_region_terminated`
    /// afterwards; doing so would be a double-termination error.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion`
    /// when there is no active region; `VariableNotFound` when
    /// any element of `ret_vars` is not tracked; `ExpectedControl`
    /// or `ExpectedMemory` if the region's snapshotted ctrl/mem
    /// edges are mistyped (graph-construction bug); or
    /// `ExpectedValue` when `value` or any read return register
    /// is not a value edge.
    pub fn build_return(
        &mut self,
        value: Option<ValueId>,
        ret_vars: &[rsleigh::Vn],
    ) -> Result<()> {
        let mut ret_inputs: SmallVec<[ValueId; 4]> = SmallVec::new();
        if let Some(v) = value {
            ret_inputs.push(v);
        }
        for var in ret_vars {
            require_reg_or_unique(var)?;
            ret_inputs.push(self.read_reg_vn(var)?);
        }

        // Terminate the region and snapshot ctrl/mem in one step.
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

    /// Emits a function-ABI `Return` node whose value slots are the
    /// function's calling-convention return registers, in ABI order.
    /// This is the canonical RET lowering: the caller no longer threads
    /// the return-register list — it is read from the function's
    /// resolved CC ([`crate::Function::ret_val_regs`]).
    ///
    /// Like [`Self::build_return`], this **terminates** the current
    /// region unconditionally.  Callers must not call
    /// `mark_cur_region_terminated` afterwards.
    ///
    /// The synthetic single-value return path
    /// ([`Self::build_return`] with an explicit `Some(value)` and no
    /// `ret_vars`) is intentionally kept separate.
    ///
    /// # Errors
    ///
    /// Same as [`Self::build_return`].
    pub fn build_function_return(&mut self) -> Result<()> {
        // Clone the ABI return-register list out so the subsequent
        // `&mut self` reads in `build_return` don't alias the borrow.
        let ret_vars: SmallVec<[rsleigh::Vn; 4]> =
            self.function.ret_val_regs().into_iter().collect();
        self.build_return(None, &ret_vars)
    }

    /// Terminates the current region with an `IndirectBranch` placeholder
    /// node anchoring `target_value`, returning the created node's
    /// [`NodeId`].  Inputs: `[control, memory, target_value]`.  Outputs:
    /// `[]`.  The returned id lets the lifter correlate the placeholder
    /// with its pcode address so the resolver can key its classification
    /// back to the dispatch site.
    ///
    /// Used by the lifter when the CFG terminator is
    /// `RegionTerminator::UnresolvedIndirectBranch`: the value at the
    /// dispatch site is anchored as a value-typed slot on the placeholder
    /// so the indirect-branch resolver can later inspect its producer and
    /// either rewrite the placeholder into a real `Return` or splice in a
    /// `Call`+`Return` pair.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated` when there is no
    /// active region; `ExpectedControl` or `ExpectedMemory` if the
    /// region's snapshotted control/memory edges are mistyped
    /// (graph-construction bug); or `ExpectedValue` when `target_value`
    /// is not a value edge.
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
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedControl` /
    /// `ExpectedMemory` when the region's snapshotted edges are
    /// mistyped (graph-construction bug).
    pub fn build_branch(&mut self, dest: RegionId) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        self.link_region(dest, res.control, res.memory, res.region_id)
    }

    /// Terminates the current region with a conditional branch.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedValue` when
    /// `cond` is not a `Bool` value; `ExpectedControl` when the
    /// region's snapshotted control edge is mistyped;
    /// `WrongOutputCount` from the freshly created `If` node.
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

    /// Emits a `Store` node writing `data` to `addr` in `space` and advances
    /// the region's memory token.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedMemory` /
    /// `ExpectedValue` when the memory, address, or data edge is
    /// mistyped.
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

    /// Emits a `Load` node reading from `addr` in `space` and returns the
    /// loaded value output.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated`
    /// when there is no active region; `ExpectedMemory` when the
    /// memory edge is mistyped; `ExpectedValue` when `addr` is
    /// not a value edge.
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

    /// Emits a `Phi` node tagged with varnode `var` via the
    /// `value_vn` side-table.
    ///
    /// `phi_token` must be the `PhiToken` output of the owning `Region`.
    /// `incoming_values` are the data inputs, one per predecessor (may be empty
    /// when first created; filled in later via `add_region_predecessor`).
    pub(super) fn build_vn_phi(
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
