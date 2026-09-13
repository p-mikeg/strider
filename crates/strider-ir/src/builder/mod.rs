use cranelift_entity::PrimaryMap;
use cranelift_entity::packed_option::ReservedValue;

use crate::error::Result;
use crate::function::Function;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind};
use crate::region::Region;

mod build_trait;
pub use build_trait::IRBuilder;
mod builder_ext;
pub use builder_ext::IRBuilderExt;
mod call;
mod nodes;
#[cfg(any(test, feature = "test-util"))]
mod test_support;
#[cfg(test)]
mod tests;
mod vars;

/// `VnSpace` renders as its shortcut character alone, and ram's is `r`, which
/// reads as "register" in a message rejecting one.
fn space_name(space: rsleigh::VnSpace) -> &'static str {
    match space {
        rsleigh::VnSpace::REGISTER => "register",
        rsleigh::VnSpace::UNIQUE => "unique",
        rsleigh::VnSpace::RAM => "ram",
        rsleigh::VnSpace::CONST => "const",
        _ => "an unnamed",
    }
}

/// Errors unless `vn` is in REGISTER or UNIQUE space.
pub(super) fn require_reg_or_unique(vn: &rsleigh::Vn) -> crate::error::Result<()> {
    match vn.addr_space {
        rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE => Ok(()),
        space => Err(anyhow::anyhow!(
            "a call-class read or write names a {} varnode at {:#x} size {}; \
             only register and unique varnodes can carry one",
            space_name(space),
            vn.addr_off,
            vn.size,
        )),
    }
}

/// Tracks SSA-style per-region variable state: each variable has exactly one
/// current `ValueId` inside the active region, and every read and write goes
/// through that mapping.
pub struct FunctionBuilder {
    pub(crate) function: Function,
    /// The single `Memory` output of the `InitialMemory` node.
    pub(crate) entry_memory: ValueId,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    /// Stamped onto every node `create_node` produces while it is `Some`.
    lift_addr: Option<u64>,
    /// What each `Call`'s clobbered registers held when it was built, at the
    /// stack pointer's width.
    call_register_values: Vec<ValueId>,
}

impl FunctionBuilder {
    pub fn function(&self) -> &Function {
        &self.function
    }

    pub fn function_mut(&mut self) -> &mut Function {
        &mut self.function
    }

    pub fn entry(&self) -> NodeId {
        self.function.entry()
    }

    /// The sole constructor. `all_used_variables` is every varnode appearing
    /// in the function.
    pub fn new(
        mut all_used_variables: Vec<rsleigh::Vn>,
        cc: strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
    ) -> Result<Self> {
        // Callers pass the stack vn in `all_used_variables` themselves.
        for v in cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
            .chain(cc.arg_passing_regs_float.iter())
        {
            if !all_used_variables.contains(v) {
                all_used_variables.push(*v);
            }
        }
        let tracked_vns = vn_container::dedup_overlapping_largest(&all_used_variables);
        let mut fb = FunctionBuilder {
            function: Function::new(cc, endianness, tracked_vns),
            entry_memory: ValueId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
            lift_addr: None,
            call_register_values: Vec::new(),
        };
        fb.build_entry()?;
        Ok(fb)
    }

    /// Attributes every subsequent `create_node` to `addr` until replaced.
    #[inline]
    pub fn set_lift_addr(&mut self, addr: Option<u64>) {
        self.lift_addr = addr;
    }

    /// The machine address currently being lifted, if any.
    #[inline]
    pub fn lift_addr(&self) -> Option<u64> {
        self.lift_addr
    }

    /// Stamps the current lift address into the node's asm-fingerprint. On a
    /// dedup-cache hit the address is unioned into the existing entry.
    pub(crate) fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        let addr = self.lift_addr;
        let node_id = self
            .function_mut()
            .create_node_attributed(kind, inputs, output_kinds, &[]);
        if let Some(addr) = addr {
            self.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint(node_id, &[addr]);
        }
        node_id
    }

    /// Validates before handing the function over. A failure wraps a
    /// [`crate::validate::ValidationReport`], recoverable with
    /// `err.downcast_ref::<crate::validate::ValidationReport>()`.
    pub fn build(mut self) -> crate::Result<crate::Function> {
        crate::validate::validate(&self.function).map_err(|e| e.report(&self.function))?;
        if any_stack_derived(&self.function, &self.call_register_values) {
            self.function
                .side_tables_mut()
                .set_frame_address_in_call_register();
        }
        Ok(self.function)
    }
}

/// Whether any of `values` is computed from the entry stack pointer through the
/// arithmetic an address is built from: `Add`, `And`, `Or`, `Xor`, a width
/// change, or a `Phi`.  A value reloaded from memory is left out, since storing
/// the address was already an escape.
fn any_stack_derived(function: &Function, values: &[ValueId]) -> bool {
    use crate::IRViewer;
    let sp = function.stack_vn();
    let mut seen = entity_utils::DenseEntitySet::new();
    let mut work = values.to_vec();
    while let Some(v) = work.pop() {
        if !seen.insert(v) {
            continue;
        }
        let node = function.producer(v);
        match *function.node_kind(node) {
            NodeKind::InitialVar(id) if function.initial_vn(id) == sp => return true,
            NodeKind::IntBinaryOp(
                IntBinaryOp::Add | IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor,
            )
            | NodeKind::Extend(_)
            | NodeKind::Truncate
            | NodeKind::Phi => work.extend(function.value_inputs(node)),
            _ => {}
        }
    }
    false
}
