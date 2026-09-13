//! Store-to-load forwarding.
//!
//! Forwards a store's value to a later load only when the nearest
//! may-aliasing memory definition is an exact-match store: same address
//! class, base and offset, with stored bytes fully covering the load's
//! range (a wider store is reshaped via `Truncate` / `ShiftRight`).
//! Anything else blocks: a non-exact overlapping store, a `MemPhi`
//! (control merge, arms may disagree), or `InitialMemory`.
//!
//! A `Call` blocks unless its convention declares `preserves_memory`; a
//! `CallOther` always blocks, since only a user-op its ABI row declares
//! `clobbers_memory` reaches the chain.  A `Call` also steps through when
//! `escape_analysis`
//! proves the frame private and the slot is outside the call's
//! outgoing-argument window, and when it is a listed `noalias_allocators`
//! callee and the probed location is a PRIVATE-frame stack slot outside that
//! same window, or a different allocation.
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
    analyzers: RefCell<Option<Analyzers>>,
}

/// `alias` carries the run's claims and decides whether to forward; `narrow`
/// carries none, and is the only one a permanent rewire may name.
struct Analyzers {
    alias: MemAnalyzer,
    narrow: MemAnalyzer,
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
        *self.analyzers.borrow_mut() = None;
    }

    // A fresh forward reaches a consumer only through an address edge, which
    // the next pipeline iteration picks up; re-enqueueing every consumer to
    // catch it costs more than the deferral.
    fn propagate_to_consumers(&self) -> bool {
        false
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<crate::peephole::PeepholeRewrite> {
        let mut analyzers = self.analyzers.borrow_mut();
        let cfgs = analyzers.get_or_insert_with(|| {
            let assumptions = &opt_ctx.options.assumptions;
            // `call_blocking`: a store at another SP base may still alias.
            let relaxed = MemOptions::call_blocking(
                assumptions.stack_global_disjoint,
                &assumptions.noalias_allocators,
            )
            .with_escape_analysis(assumptions.escape_analysis)
            .with_callee_preserves_stack_args(assumptions.callee_preserves_stack_args);
            Analyzers {
                alias: MemAnalyzer::new(relaxed),
                // `narrow_load_to` rewires for good, so every claim is off:
                // the edge outlives the run that made it.
                narrow: MemAnalyzer::new(MemOptions::structural()),
            }
        });
        Ok(crate::peephole::PeepholeRewrite::from_changed(
            try_forward_load(edit, root, &cfgs.alias, &cfgs.narrow)?.changed(),
        ))
    }
}

/// QUADRATIC on the FIRST sweep: `nearest_clobber` is an unmemoised reverse
/// walk as long as the distance from the load to its defining store, it runs
/// twice per load, and its memo is keyed on the probed location, so loads at
/// different offsets share nothing.  The cost is loads x memory-chain length.
/// `narrow_load_to` shortens only the NARROW walk, onto the structural
/// clobber; the alias walk keeps stepping past it under the relaxations the
/// structural options pin off, so its cost is the relaxation-visible chain on
/// every sweep.
///
/// What bounds it in practice is that a `Call` ends the chain, so optimised
/// input never builds a long one.  A call-free run of frame traffic does:
/// `-O0` output, large leaf functions, big register-spill regions.
///
/// Answering in one pass needs a per-location def index, which a `MemPhi` DAG
/// has no linear order to build one over; that is a MemorySSA redesign, not a
/// tuning knob.
fn try_forward_load(
    edit: &mut crate::EditFunction<'_>,
    load: NodeId,
    alias_cfg: &MemAnalyzer,
    narrow_cfg: &MemAnalyzer,
) -> Result<OptimizationResult> {
    let mem = edit
        .memory_input_of(load)
        .expect("a Load has a memory input (slot 0)");
    let (load_value, load_ty) = edit.single_value_output(load)?;

    // First: `narrow_cfg` stops at more clobbers, so it only reaches addresses
    // this walk has already decomposed under the configured allocator set, and
    // its empty set never commits a `NotMemory` for them (see `decompose`).
    let clobber_node = alias_cfg.nearest_clobber(edit.function(), load, mem);
    // Shorten the load's memory edge onto the clobber `narrow_cfg` proves so
    // future walks skip the proven-disjoint run.  Never `alias_cfg`'s: the
    // rewire outlives this run, and a later run with the relaxations off would
    // inherit an edge that only holds with them on.
    let sound_clobber = narrow_cfg.nearest_clobber(edit.function(), load, mem);
    crate::mem_ssa::narrow_load_to(edit, load, sound_clobber);

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
