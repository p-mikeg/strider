//! Store-to-load forwarding.
//!
//! Forwards a store's value to a later load only when the nearest
//! may-aliasing memory definition is an exact-match store: same address
//! class, base and offset, with stored bytes fully covering the load's
//! range (a wider store is reshaped via `Truncate` / `ShiftRight`).
//! Anything else blocks: a non-exact overlapping store, a `MemPhi`
//! (control merge, arms may disagree), a `CallOther`, or `InitialMemory`.
//! A `Call` blocks too unless `escape_analysis` proves the frame private.
//!
//! The pass never synthesizes a value-`Phi`; a control merge is opaque.
//! Requires `PhiCollapse` to have run, so a trivial `MemPhi` is already gone.

use std::cell::RefCell;

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};
use strider_ir::{IRBuilderExt, IRViewer};
use strider_target::Endianness;

use crate::error::Result;
use crate::mem_analysis::{AliasVerdict, MemAnalyzer, MemOptions};
use crate::pipeline::OptimizationResult;

#[derive(Default)]
pub struct LoadForward {
    /// Shared by every root in one sweep, for its outgoing-argument-window
    /// memo.  Valid that long because the pass only rewires `Load` memory
    /// edges and redirects `Load` outputs: a `Call` keeps its memory input and
    /// its SP, and a forwarded value can only make an address the window scan
    /// could not place placeable, which ends a prefix sooner than the memo
    /// says.
    analyzer: RefCell<Option<MemAnalyzer>>,
}

impl Clone for LoadForward {
    /// The memo belongs to one sweep of one graph, so a clone starts empty.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl crate::peephole::PeepholePass for LoadForward {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    fn start_sweep(&self) {
        *self.analyzer.borrow_mut() = None;
    }

    // A Load's output never feeds another Load's memory slot, so a rewrite
    // exposes no new forward.
    fn propagate_to_consumers(&self) -> bool {
        false
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<crate::peephole::PeepholeRewrite> {
        // `call_blocking`: a store at another SP base may still alias.
        let options = MemOptions::call_blocking(opt_ctx.options.alias_mode)
            .with_escape_analysis(opt_ctx.options.assumptions.escape_analysis)
            .with_callee_preserves_stack_args(
                opt_ctx.options.assumptions.callee_preserves_stack_args,
            );
        let mut analyzer = self.analyzer.borrow_mut();
        let alias_cfg = analyzer.get_or_insert_with(|| MemAnalyzer::new(options));
        Ok(crate::peephole::PeepholeRewrite::from_changed(
            try_forward_load(edit, root, alias_cfg)?.changed(),
        ))
    }
}

fn try_forward_load(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    alias_cfg: &MemAnalyzer,
) -> Result<OptimizationResult> {
    let mem = edit
        .memory_input_of(load)
        .expect("a Load has a memory input (slot 0)");
    let (load_value, load_ty) = edit.single_value_output(load)?;

    let clobber_node = alias_cfg.nearest_clobber(edit.function(), load, mem);
    // Shorten the load's memory edge onto its nearest clobber so future walks
    // skip the proven-disjoint run.  Harmless if the load then forwards away.
    crate::mem_ssa::narrow_load_to(edit, load, clobber_node);

    if !matches!(edit.node_kind(clobber_node), NodeKind::Store(_)) {
        return Ok(OptimizationResult::NoChange);
    }

    // Exact match: same location, stored bytes covering the load's range.
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
        return Ok(OptimizationResult::NoChange);
    };

    // Redirecting the sole output leaves the Load dead; the automatic cull
    // removes it and its address cone.
    let changed = edit.replace_value(load_value, forwarded)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Reshapes a wider store's value down to the load width.  On BE the load's
/// bytes are the high ones, so a `(store_size - load_size) * 8` shift precedes
/// the truncate.
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
            let shift_bits =
                crate::mem_analysis::high_low_shift_bits(store_data_ty, load_ty, endianness);
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
