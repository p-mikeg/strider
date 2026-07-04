use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::IRViewer;
use crate::builder::IRBuilderExt;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use crate::region::RegionId;

impl FunctionBuilder {
    /// Builds the function's `InitialMemory` node and captures its starting
    /// memory token as the builder's `entry_memory`.
    ///
    /// [`Function::new`] builds only the `Entry` node; the memory spine is the
    /// builder's responsibility, so this creates the `InitialMemory` node
    /// (an asm-fingerprint-exempt initial-state kind, minted straight on the
    /// graph) and records its single `Memory` output — no graph search.
    ///
    /// # Errors
    ///
    /// Returns `WrongOutputCount` if the freshly-built `InitialMemory` node does
    /// not have its expected single output (a graph-construction bug).
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

    /// Emits a `Return` node into the current region from already-resolved
    /// return-value inputs.
    ///
    /// Terminates the current region with a `Return` node whose value
    /// slots are the explicitly-provided `value` (when `Some`) followed
    /// by `ret_values` in order.  This is a **dumb** node emitter: the
    /// caller (the lifter) resolves the calling-convention return registers
    /// and reads them through its own aliasing-aware `read_vn` before
    /// handing the resulting values here — strider-ir knows nothing about
    /// which varnodes those values came from.
    ///
    /// This method **terminates** the current region unconditionally —
    /// callers must not terminate it again afterwards; doing so would
    /// be a double-termination error.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion`
    /// when there is no active region; `ExpectedControl`
    /// or `ExpectedMemory` if the region's snapshotted ctrl/mem
    /// edges are mistyped (graph-construction bug); or
    /// `ExpectedValue` when `value` or any element of `ret_values`
    /// is not a value edge.
    pub fn build_return(&mut self, value: Option<ValueId>, ret_values: &[ValueId]) -> Result<()> {
        let mut ret_inputs: SmallVec<[ValueId; 4]> = SmallVec::new();
        if let Some(v) = value {
            ret_inputs.push(v);
        }
        ret_inputs.extend_from_slice(ret_values);

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

    /// Terminates the current region with an `Unreachable` node, sinking the
    /// region's control edge.
    ///
    /// This is the control-sink for a no-return direct `Call` — a call whose
    /// fall-through (return address) lies outside the function bound because
    /// the callee never returns (e.g. FreeBSD `exit1`/`__dead2`). It is the
    /// direct-`Call` analogue of the `Unreachable` that [`Self::build_call_other`]
    /// emits for the NoReturn `CallOther` class. The memory edge is
    /// intentionally left dangling — `Unreachable` consumes only control.
    ///
    /// This method **terminates** the current region unconditionally — callers
    /// must not terminate it again afterwards.
    ///
    /// # Errors
    ///
    /// Returns `NoCurrentRegion` / `RegionTerminated` when there is no active
    /// region; `ExpectedControl` or `ExpectedMemory` if the region's
    /// snapshotted control/memory edges are mistyped (graph-construction bug).
    pub fn build_unreachable(&mut self) -> Result<()> {
        let res = self.terminate_cur_region()?;
        self.require_terminator_kinds(&res)?;
        self.create_node(NodeKind::Unreachable, [res.control], []);
        Ok(())
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
        // A phi's source varnode is always a tracked variable, so it has a
        // `VnId`; tag the phi output with it (set_vn_for_value no-ops on the
        // impossible untracked case, matching the Call/CallOther path).
        self.function_mut().set_vn_for_value(phi_value, var);
        Ok(phi_value)
    }
}
