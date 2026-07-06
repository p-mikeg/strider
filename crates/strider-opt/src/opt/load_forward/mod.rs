//! Forwards the value of a `Store(addr=sp+K)` to a subsequent
//! `Load[sp + K]` when the live (nearest may-aliasing) memory definition
//! reaching the load is an **exact-match store** to the same location.
//!
//! The pass walks the memory-SSA chain backward from the load via the
//! pass-scoped [`crate::sp_expr::SpAliasCfg`]
//! ([`nearest_clobber`][crate::sp_expr::SpAliasCfg::nearest_clobber],
//! `call_clobbers: true` — a load never forwards across a call), which
//! supplies the per-def aliasing verdict.
//! The walker returns the nearest may-aliasing definition NODE:
//!
//! * a `Store` to the SAME location (address class + base + offset) whose
//!   value covers the load's byte range → forward the stored value
//!   (reshaping a wider store with `Truncate` / `ShiftRight` as needed);
//! * a `Store` that overlaps but is NOT an exact match, a `MemPhi`
//!   (control-merge boundary — the branches disagree on the live value),
//!   a `Call` / `CallOther`, or the `InitialMemory` node (clean chain) →
//!   do NOT forward.
//!
//! The pass NEVER synthesizes a value-`Phi`: a control merge is an opaque
//! boundary, so a load whose live def is a disagreeing `MemPhi` simply
//! stays.  (A trivial `MemPhi` whose arms all carry the same memory token
//! is collapsed by `PhiCollapse` before this pass runs, so the dominating
//! store behind such a merge is still reached and forwarded.)
//!
//! The stack-pointer varnode and target endianness are read from the
//! function under analysis (`Function::default_cc` / `Function::endianness`),
//! so the pass takes no convention configuration.

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};
use strider_ir::{IRBuilderExt, IRViewer};
use strider_target::Endianness;

use crate::error::Result;
use crate::pipeline::OptimizationResult;
use crate::sp_expr::{AliasVerdict, SpAliasCfg};

/// Store-to-load forwarding for SP-relative stack slots.
///
/// Runs inside the main fixed-point loop so that stack stores classified by
/// `StackOffsetDetect` become visible to the walker on subsequent iterations,
/// and so that forwarded constants fed into expressions are in turn
/// simplified by `ConstantFold` / `KnownBits`.
///
/// The stack-pointer varnode and target endianness are read from the
/// function under analysis (`Function::default_cc` / `Function::endianness`)
/// at apply time, and the alias-analysis precision is read from the shared
/// per-run [`AliasMode`][crate::AliasMode] — the pass carries no configuration of its
/// own.
#[derive(Clone)]
pub struct LoadForward;

impl crate::peephole::PeepholePass for LoadForward {
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    // A `Load` is never another load's operand, so a forwarded load needs no
    // consumer re-enqueue.
    fn propagate_to_consumers(&self) -> bool {
        false
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<crate::peephole::PeepholeRewrite> {
        // SP decompositions are recomputed read-only off the live graph (the
        // `stack_offsets` cache is empty during the fixed point), so no memo is
        // threaded.
        let alias_mode = opt_ctx.options.alias_mode;
        Ok(crate::peephole::PeepholeRewrite::from_changed(
            try_forward_load(ctx, root, alias_mode)?.changed(),
        ))
    }
}

/// Tries to forward a single `Load` to the value of its live upstream
/// `Store`.  Finds the nearest may-aliasing memory definition via the
/// pass-scoped [`SpAliasCfg`]; forwards iff that definition is an
/// exact-match `Store`.  Returns `Changed` iff the load's uses were rewired.
fn try_forward_load(
    ctx: &mut crate::EditFunction<'_>,
    load: NodeId,
    alias_mode: crate::AliasMode,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr]; only the memory token is needed here — the
    // load's address class / byte size are re-derived from the node by the
    // cfg's `nearest_clobber` / `verdict` helpers (each an O(1) cached read).
    let mem = ctx
        .memory_input_of(load)
        .expect("a Load has a memory input (slot 0)");
    // A `Load` always produces a single value output (validated signature).
    let (load_value, load_ty) = ctx.single_value_output(load)?;

    // load_forward stays conservative on distinct SP bases (a store at a
    // different SP base may still alias the forwarded load); a `Call` always
    // blocks a forward (`call_clobbers: true`).
    let alias_cfg = SpAliasCfg::call_blocking(alias_mode);

    // 1. Find the nearest definition that may alias the load.  A clean
    //    chain returns the `InitialMemory` node (handled by the Store
    //    check below) → nothing to forward.  A `Call` always blocks a
    //    forward (`call_clobbers: true`).
    let clobber_node = alias_cfg.nearest_clobber(ctx.function(), load, mem);
    // Narrowing is now a caller-side step: shorten this load's memory edge
    // onto its nearest clobber so future walks skip the proven-disjoint run.
    // (Harmless when the load goes on to forward — it's culled either way.)
    crate::sp_expr::narrow_load_to(ctx, load, clobber_node);

    // 2. The clobber must be a `Store`.  A `MemPhi` boundary (disagreeing
    //    control merge), a `Call` / `CallOther`, `InitialMemory` (clean
    //    chain), or any opaque producer is NOT forwardable.
    if !matches!(ctx.node_kind(clobber_node), NodeKind::Store(_)) {
        return Ok(OptimizationResult::NoChange);
    }

    // 3. Exact-match check: the store must write the SAME location (address
    //    class + base + offset) and its value must cover the load's byte
    //    range.  `cfg.verdict` derives both sides' class + size from the nodes;
    //    an overlapping-but-shifted store is not forwardable.
    if alias_cfg.verdict(ctx.function(), load, clobber_node) != AliasVerdict::Match {
        return Ok(OptimizationResult::NoChange);
    }

    // 4. Forward the stored value, reshaping a wider store down to the
    //    load width when needed.  These are value-reshaping nodes
    //    (`Truncate` / `ShiftRight`), never a `Phi`.
    let store_data = ctx.store_data(clobber_node);
    // A `Store`'s data input is an `AnyInt` value slot (validated), so its
    // source output is always a value.
    let store_data_ty = ctx
        .value_type_opt(store_data)
        .expect("Store data input is a value");
    let forwarded = if store_data_ty == load_ty {
        store_data
    } else if store_data_ty.is_integer()
        && load_ty.is_integer()
        && load_ty.byte_size() < store_data_ty.byte_size()
        // The BE reshape mints a shift const at `store_data_ty` via
        // `build_int_const`, which only materialises types up to I128
        // (I256/I512 route through the wide interner separately).  Bail
        // (NoChange) on a wider store rather than fail the pass — such a
        // wide-store→narrow-load forward is exotic.
        && store_data_ty.byte_size() <= 16
    {
        narrow(ctx, store_data, load)?
    } else {
        // Same offset but the stored bytes do not fully back the load
        // (narrower store, or a non-integer reshape) → cannot forward.
        return Ok(OptimizationResult::NoChange);
    };

    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // forwarded producer and redirects all uses.  The reshaping nodes built
    // in `narrow` are each attributed via `create_node_attributed(..,
    // &[load])`, so the contract holds at every intermediate node.
    //
    // A `Load` is not side-effecting and has a single (value) output, so
    // redirecting that output leaves the Load at zero uses — `replace_value`
    // already enqueued its producer, and the automatic `clean()` cull then
    // removes the Load and cascade-culls its now-dead address cone.  No
    // manual detach is needed; the memory chain is Store/MemPhi/Call-only, so
    // a still-attached forwarded Load never pollutes the memory-SSA walk in
    // the same sweep.
    let changed = ctx.replace_value(load_value, forwarded)?;
    Ok(OptimizationResult::from_changed(changed))
}

/// Synthesises a narrow-load-from-wider-store reshape and returns the
/// reshaped value's output id.
///
/// - LE: load bytes are the low `load_size` bytes of the stored value →
///   `Truncate(data)`.
/// - BE: load bytes are the high `load_size` bytes →
///   `Truncate(ShiftRight(data, (store_size - load_size) * 8))`.  The
///   logical (zero-fill) `ShiftRight` positions the high bytes in the low
///   end before truncating.
///
/// Every freshly-synthesised node is built via
/// `create_node_attributed(.., &[load])` so the asm-fingerprint contract
/// holds at every intermediate node, not just the outermost — the
/// BE-path `ShiftRight` / `IntConst` would otherwise be reachable with an
/// empty fingerprint.
fn narrow(ctx: &mut crate::EditFunction<'_>, store_data: ValueId, load: NodeId) -> Result<ValueId> {
    // Both `store_data_ty` (the `Store` data input) and `load_ty` (the `Load`
    // output) are value-edge types, so derive them from the nodes the caller
    // already holds — each is an O(1) cached look-up — rather than threading
    // them in as redundant decomposed arguments.
    let store_data_ty = ctx.value_type(store_data)?;
    let (_, load_ty) = ctx.single_value_output(load)?;
    // SSoT: the byte order is the function's own.
    let endianness = ctx.function().endianness();
    let shifted = match endianness {
        Endianness::Little => store_data,
        Endianness::Big => {
            // SSoT for the endianness-aware byte-slice shift (shared with the
            // jump-table evaluator's symbolic `reshape`).
            let shift_bits =
                crate::sp_expr::high_low_shift_bits(store_data_ty, load_ty, endianness);
            // shift_bits is a byte-offset * 8 — always fits in u128.  Route it
            // through `build_int_const` so a wide `store_data_ty` (I80 / I128)
            // mints the interned const so it dedups correctly against any other
            // node with the same value and type.
            let shift_const = ctx.build_int_const(u128::from(shift_bits), store_data_ty)?;
            // `build_int_const` does not carry the `&[load]` contributor stamp
            // that `create_node_attributed` would, so attribute the fresh const
            // to the load explicitly — every reachable node must carry ≥1
            // asm-fingerprint.
            let shift_const_node = ctx.producer(shift_const);
            ctx.function_mut()
                .side_tables_mut().extend_asm_fingerprint_from(shift_const_node, load);
            let shr = ctx.create_node_attributed(
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
                [store_data, shift_const],
                [ValueKind::Typed(store_data_ty)],
                &[load],
            );
            let [value] = ctx.node_outputs_exact::<1>(shr)?;
            value
        }
    };
    let trunc = ctx.create_node_attributed(
        NodeKind::Truncate,
        [shifted],
        [ValueKind::Typed(load_ty)],
        &[load],
    );
    let [value] = ctx.node_outputs_exact::<1>(trunc)?;
    Ok(value)
}

#[cfg(test)]
mod tests;
