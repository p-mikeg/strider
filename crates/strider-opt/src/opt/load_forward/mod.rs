//! Store-to-load forwarding.
//!
//! Forwards a store's value to a later load only when the nearest
//! may-aliasing memory definition is an exact-match store: same address
//! class, base and offset, with stored bytes fully covering the load's
//! range (a wider store is reshaped via `Truncate` / `ShiftRight`).
//! Anything else blocks: a non-exact overlapping store, a `MemPhi` whose arms
//! reach different definitions, or `InitialMemory`.  A `MemPhi` whose arms
//! agree is transparent.
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
//! The pass never synthesizes a value-`Phi`, so no merge of two stored values
//! forwards.  A stack address decomposes only once `PhiCollapse` has folded the
//! region phis over SP.

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};
use strider_ir::{IRBuilderExt, IRViewer};
use strider_target::Endianness;

use crate::error::Result;
use crate::mem_analysis::{AliasVerdict, MemAnalyzer, MemOptions, MemPurpose};
use crate::pipeline::OptimizationResult;

#[derive(Clone, Copy, Default)]
pub struct LoadForward;

/// One sweep's analyzers, shared by every root for their memory index,
/// answers and outgoing-argument-window memo.
///
/// The pass only rewires `Load` memory edges and redirects `Load` outputs, so
/// every memory def keeps its inputs and the index stands.  A store address a
/// forward can still change was an `Anchor`, which the index offers every
/// location, and a forward that reaches an address drops the answers.  A
/// forwarded value only makes an address the window scan could not place
/// placeable, which ends a prefix sooner than the memo says.
///
/// `alias` carries the function's claims and decides whether to forward;
/// `narrow` carries them less the call relaxations, and is the only one a
/// permanent rewire may name.
pub(crate) struct Analyzers {
    alias: MemAnalyzer,
    narrow: MemAnalyzer,
}

// Not `PROPAGATE`: a fresh forward reaches a consumer only through an address
// edge, which the next pipeline iteration picks up; re-enqueueing every
// consumer to catch it costs more than the deferral.
impl crate::peephole::PeepholePass for LoadForward {
    type Sweep = Option<Analyzers>;

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    fn try_rewrite(
        &self,
        analyzers: &mut Option<Analyzers>,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<crate::peephole::PeepholeRewrite> {
        let cfgs = analyzers.get_or_insert_with(|| {
            let function = edit.function();
            Analyzers {
                alias: MemAnalyzer::new(MemOptions::for_purpose(MemPurpose::Forward, function)),
                // `narrow_load_to` rewires for good, and the walks that read
                // the edge afterwards honour no call relaxation (the jump-table
                // walk, return-target verification with its claim off), so the
                // narrowing honours every claim but those.
                narrow: MemAnalyzer::new(MemOptions::for_purpose(MemPurpose::TableWalk, function)),
            }
        });
        Ok(crate::peephole::PeepholeRewrite::from_changed(
            try_forward_load(edit, root, &cfgs.alias, &cfgs.narrow)?.changed(),
        ))
    }
}

/// Two nearest-clobber queries per load, each a climb of the memory dominator
/// tree over the analyzers' index, or a path walk where the memory graph is
/// irreducible and has none.  `narrow_load_to` moves the load's edge onto the
/// structural clobber, which later sweeps start from.
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

    let clobber_node = alias_cfg.nearest_clobber(edit.function(), load, mem);
    // Shorten the load's memory edge onto the clobber `narrow_cfg` proves so
    // future walks skip the proven-disjoint run.  Never `alias_cfg`'s: the
    // stricter walks that read the edge afterwards would inherit a relaxation
    // they do not honour.
    let sound_clobber = narrow_cfg.nearest_clobber(edit.function(), load, mem);
    // A moved edge is a change: the load may now be a structural twin of
    // another, and the fixed point must not exit before that merge is seen.
    let narrowed = crate::mem_ssa::narrow_load_to(edit, load, sound_clobber);
    let unforwarded = OptimizationResult::from_changed(narrowed);

    if !matches!(edit.node_kind(clobber_node), NodeKind::Store(_)) {
        return Ok(unforwarded);
    }

    // Exact match: same location, stored bytes covering the load's range.
    if alias_cfg.verdict(edit.function(), load, clobber_node) != AliasVerdict::Match {
        return Ok(unforwarded);
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
        return Ok(unforwarded);
    };

    // A forwarded address can turn an `Anchor` store into a located one,
    // which moves verdicts the analyzers memoised.
    if feeds_an_address(edit.function(), load_value) {
        alias_cfg.forget_answers();
        narrow_cfg.forget_answers();
    }
    // Redirecting the sole output leaves the Load dead; the automatic cull
    // removes it and its address cone.
    let changed = edit.replace_value(load_value, forwarded);
    Ok(OptimizationResult::from_changed(changed || narrowed))
}

/// Whether `value` reaches a store's address or a call's SP through `Add` and
/// `And` steps, the shapes `decompose` follows.
fn feeds_an_address(function: &strider_ir::Function, value: ValueId) -> bool {
    let mut work = vec![value];
    let mut seen = rustc_hash::FxHashSet::default();
    while let Some(v) = work.pop() {
        for (user, slot) in function.graph().value_uses(v) {
            match function.node_kind(user) {
                NodeKind::Store(_) if slot == 1 => return true,
                NodeKind::Call { .. } if slot == 3 => return true,
                NodeKind::IntBinaryOp(
                    strider_ir::IntBinaryOp::Add | strider_ir::IntBinaryOp::And,
                ) if seen.insert(user) => {
                    work.extend(function.node_outputs(user).iter().copied());
                }
                _ => {}
            }
        }
    }
    false
}

/// Reshapes a wider store's value down to the load width, in the load's space's
/// byte order.  On BE the load's bytes are the high ones, so a
/// `(store_size - load_size) * 8` shift precedes the truncate.
fn narrow(
    edit: &mut crate::EditFunction<'_>,
    store_data: ValueId,
    load: NodeId,
) -> Result<ValueId> {
    let store_data_ty = edit.value_type(store_data)?;
    let (_, load_ty) = edit.single_value_output(load)?;
    let NodeKind::Load(space) = *edit.node_kind(load) else {
        unreachable!("narrow reshapes a load");
    };
    let endianness = edit.function().byte_order().of(space);
    let shifted = match endianness {
        Endianness::Little => store_data,
        Endianness::Big => {
            let shift_bits =
                crate::const_eval::high_low_shift_bits(store_data_ty, load_ty, endianness);
            let shift_const = edit.build_int_const(u128::from(shift_bits), store_data_ty)?;
            // `build_int_const` carries no contributor stamp, so attribute the
            // const by hand; every reachable node needs a fingerprint.
            let shift_const_node = edit.producer(shift_const);
            edit.absorb_node_fingerprint(shift_const_node, load);
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
