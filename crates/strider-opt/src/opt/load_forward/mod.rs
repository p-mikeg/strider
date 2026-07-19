//! Store-to-load forwarding.
//!
//! Forwards a store's value to a later load only when the nearest
//! may-aliasing memory definition is an exact-match store: same address
//! class, base and offset, with stored bytes fully covering the load's
//! range (a wider store is reshaped via `Truncate` / `ShiftRight`).
//! Anything else blocks: a non-exact overlapping store, a `MemPhi`
//! (control merge, arms may disagree), a `Call` / `CallOther`, or
//! `InitialMemory`.
//!
//! The pass never synthesizes a value-`Phi`; a control merge is opaque.
//! `PhiCollapse` runs first, so a trivial `MemPhi` is already gone and a
//! store dominating the merge is still reachable.

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};
use strider_ir::{IRBuilderExt, IRViewer};
use strider_target::Endianness;

use crate::error::Result;
use crate::pipeline::OptimizationResult;
use crate::sp_analysis::{AliasVerdict, SpAnalyzer, SpOptions};

/// Runs inside the main fixed-point loop: stack stores classified by
/// `StackOffsetDetect` only become visible to the walker on a later
/// iteration, and forwarded constants feed `ConstantFold` / `KnownBits`.
#[derive(Clone)]
pub struct LoadForward;

impl crate::peephole::PeepholePass for LoadForward {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    // A `Load` is never another load's operand, so no consumer re-enqueue.
    fn propagate_to_consumers(&self) -> bool {
        false
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<crate::peephole::PeepholeRewrite> {
        // SP decompositions are recomputed read-only off the live graph: the
        // `stack_offsets` cache is empty during the fixed point.
        let alias_mode = opt_ctx.options.alias_mode;
        Ok(crate::peephole::PeepholeRewrite::from_changed(
            try_forward_load(edit, root, alias_mode)?.changed(),
        ))
    }
}

fn try_forward_load(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    alias_mode: crate::AliasMode,
) -> Result<OptimizationResult> {
    let mem = edit
        .memory_input_of(load)
        .expect("a Load has a memory input (slot 0)");
    let (load_value, load_ty) = edit.single_value_output(load)?;

    // Conservative on distinct SP bases (a store at another base may still
    // alias); `call_blocking` makes any `Call` stop the walk.
    let alias_cfg = SpAnalyzer::new(SpOptions::call_blocking(alias_mode));

    let clobber_node = alias_cfg.nearest_clobber(edit.function(), load, mem);
    // Shorten the load's memory edge onto its nearest clobber so future walks
    // skip the proven-disjoint run.  Harmless if the load then forwards away.
    crate::mem_ssa::narrow_load_to(edit, load, clobber_node);

    // Only a `Store` is forwardable: `MemPhi` (arms may disagree), `Call` /
    // `CallOther`, `InitialMemory`, and opaque producers all block.
    if !matches!(edit.node_kind(clobber_node), NodeKind::Store(_)) {
        return Ok(OptimizationResult::NoChange);
    }

    // Same location and the stored bytes cover the load's range; an
    // overlapping-but-shifted store is not forwardable.
    if alias_cfg.verdict(edit.function(), load, clobber_node) != AliasVerdict::Match {
        return Ok(OptimizationResult::NoChange);
    }

    let store_data = edit.store_data(clobber_node);
    let store_data_ty = edit
        .value_type_opt(store_data)
        .expect("Store data input is a value");
    let forwarded = if store_data_ty == load_ty {
        store_data
    } else if store_data_ty.is_integer()
        && load_ty.is_integer()
        && load_ty.byte_size() < store_data_ty.byte_size()
        // The BE reshape mints a shift const via `build_int_const`, which only
        // materialises up to I128.  Bail on wider stores rather than fail; a
        // wide-store-to-narrow-load forward is exotic.
        && store_data_ty.byte_size() <= 16
    {
        narrow(edit, store_data, load)?
    } else {
        // Narrower store or non-integer reshape: bytes don't back the load.
        return Ok(OptimizationResult::NoChange);
    };

    // Redirecting the sole output leaves the Load dead; the automatic cull
    // removes it and its address cone.  No manual detach needed, since the
    // memory chain holds only Store / MemPhi / Call, so a still-attached
    // forwarded Load cannot pollute the memory-SSA walk mid-sweep.
    let changed = edit.replace_value(load_value, forwarded)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Reshapes a wider store's value down to the load width.
///
/// LE: the load's bytes are the low ones, so a plain `Truncate`.
/// BE: they are the high ones, so shift right by
/// `(store_size - load_size) * 8` first.
///
/// Every synthesised node is attributed to `load`; the asm-fingerprint
/// contract applies to intermediates too, not just the outermost node.
fn narrow(
    edit: &mut crate::EditFunction<'_>,
    store_data: ValueId,
    load: NodeId,
) -> Result<ValueId> {
    let store_data_ty = edit.value_type(store_data)?;
    let (_, load_ty) = edit.single_value_output(load)?;
    let endianness = edit.function().endianness();
    let shifted = match endianness {
        Endianness::Little => store_data,
        Endianness::Big => {
            // Shared with the jump-table evaluator's symbolic reshape.
            let shift_bits =
                crate::sp_analysis::high_low_shift_bits(store_data_ty, load_ty, endianness);
            // Via `build_int_const` so a wide store type mints the interned
            // const and dedups against equal-valued nodes.
            let shift_const = edit.build_int_const(u128::from(shift_bits), store_data_ty)?;
            // `build_int_const` carries no contributor stamp, so attribute the
            // const by hand; every reachable node needs a fingerprint.
            let shift_const_node = edit.producer(shift_const);
            edit.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint_from(shift_const_node, load);
            let shr = edit.create_node_attributed(
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
                [store_data, shift_const],
                [ValueKind::Typed(store_data_ty)],
                &[load],
            );
            let [value] = edit.node_outputs_exact::<1>(shr)?;
            value
        }
    };
    let trunc = edit.create_node_attributed(
        NodeKind::Truncate,
        [shifted],
        [ValueKind::Typed(load_ty)],
        &[load],
    );
    let [value] = edit.node_outputs_exact::<1>(trunc)?;
    Ok(value)
}

#[cfg(test)]
mod tests;
