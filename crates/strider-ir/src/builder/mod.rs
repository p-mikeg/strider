use cranelift_entity::PrimaryMap;
use cranelift_entity::packed_option::ReservedValue;

use crate::error::Result;
use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind};
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

/// `Call` / `CallOther` / `Return` output registers only model fixed-offset
/// register containment. A RAM / CONST / code-space varnode there means a bug
/// or an unmodeled ABI, so fail closed rather than emit a malformed access.
pub(super) fn require_reg_or_unique(vn: &rsleigh::Vn) -> crate::error::Result<()> {
    match vn.addr_space {
        rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE => Ok(()),
        space => Err(anyhow::anyhow!(
            "varnode {vn:?} must be in REGISTER or UNIQUE space for a \
             call-class read/write (got {space:?})"
        )),
    }
}

/// Tracks SSA-style per-region variable state: each variable has exactly one
/// current `ValueId` inside the active region, and every read and write goes
/// through that mapping.
///
/// Holds only build-time scratch. All calling-convention state lives on the
/// [`Function`]'s `default_cc` plus `all_vns`, and every register-list
/// projection a `Call` / `Return` / `CallOther` needs is derived from those
/// two. Resolving an arbitrary varnode to its container is machine-register
/// knowledge the lifter owns, not the target-agnostic IR.
pub struct FunctionBuilder {
    pub(crate) function: Function,
    /// The single `Memory` output of the `InitialMemory` node.
    pub(crate) entry_memory: ValueId,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    /// Stamped onto every node `create_node` produces while it is `Some`. The
    /// region driver sets it before each pcode insn and clears it in between,
    /// so region-setup helpers run with `None` and their synthesised
    /// structural nodes legitimately keep an empty fingerprint.
    lift_addr: Option<u64>,
}

impl FunctionBuilder {
    pub fn function(&self) -> &Function {
        &self.function
    }

    /// Pairs with [`Self::entry`]: opt passes need `(function, entry)`
    /// together, since `entry` anchors the reachable-node walk the validator's
    /// local-typing check is scoped to.
    pub fn function_mut(&mut self) -> &mut Function {
        &mut self.function
    }

    /// Stable once the first region is registered, so callers may cache it
    /// across iterations.
    pub fn entry(&self) -> NodeId {
        self.function.entry()
    }

    /// The sole constructor; synthetic and test graphs go through it too.
    ///
    /// `all_used_variables` is every varnode appearing in the function. The
    /// convention supplies the argument-passing, callee-saved and
    /// stack-pointer sets; anything neither callee-saved nor SP counts as
    /// call-clobbered, and SP is rebound at each call site by an explicit
    /// `Add(sp, ret_stack_pop)`.
    ///
    /// Errors when a tracked variable's byte size has no matching
    /// `ValueType`, since entry allocates one `InitialVar` per variable.
    pub fn new(
        mut all_used_variables: Vec<rsleigh::Vn>,
        cc: strider_target::BuiltCallingConvention,
        endianness: strider_target::Endianness,
    ) -> Result<Self> {
        // Seed every CC register so a leaf function that only forwards a call
        // still tracks what the aliasing-aware read path needs, then keep only
        // the widest varnode of each aliasing chain.
        //
        // The stack vn is deliberately NOT seeded: the lifter is the SSoT for
        // adding it before calling here, and test callers pass it in
        // `all_used_variables` themselves.
        //
        // `Function::new` then sorts by `(space, offset, size)` and interns, so
        // `InitialVnId` assignment is deterministic and the i-th tracked
        // varnode lines up with the i-th `Call` clobber output.
        for v in cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
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
        };
        fb.build_entry()?;
        Ok(fb)
    }

    /// Attributes every subsequent `create_node` to `addr` until replaced.
    /// The region driver calls this before each pcode insn; region-setup
    /// helpers leave it `None`.
    #[inline]
    pub fn set_lift_addr(&mut self, addr: Option<u64>) {
        self.lift_addr = addr;
    }

    /// Stamps the current lift address into the node's asm-fingerprint. On a
    /// dedup-cache hit the address is unioned into the existing entry.
    ///
    /// Routes through [`Function::create_node_attributed`] so integer-constant
    /// canonicalisation applies on every creation path, not just
    /// `EditFunction` and the template engine.
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
    /// [`crate::validate::ValidationErrors`] bundle; recover it with
    /// `err.downcast_ref::<crate::validate::ValidationErrors>()`.
    pub fn build(self) -> crate::Result<crate::Function> {
        crate::validate::validate(&self.function)?;
        Ok(self.function)
    }
}
