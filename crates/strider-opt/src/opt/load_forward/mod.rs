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

use strider_ir::IRViewer;
use strider_ir::IRBuilderExt;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind};
use strider_target::Endianness;

use crate::error::Result;
use crate::pipeline::OptimizationResult;
use crate::sp_expr::{AliasVerdict, SpAliasCfg, SpExprMemo, alias_verdict};

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
        // The SP-decompose memo is `opt_ctx.sp_memo` — it persists across the
        // loads of one driver sweep and is fresh per `apply` (the pipeline
        // clears it after any changed pass), matching the old per-`apply`
        // local memo.
        let alias_mode = opt_ctx.options.alias_mode;
        match try_forward_load(ctx, root, &mut opt_ctx.sp_memo, alias_mode)? {
            OptimizationResult::Changed => {
                Ok(crate::peephole::PeepholeRewrite::Changed { new_node: None })
            }
            OptimizationResult::NoChange => Ok(crate::peephole::PeepholeRewrite::NoChange),
        }
    }
}

/// Tries to forward a single `Load` to the value of its live upstream
/// `Store`.  Finds the nearest may-aliasing memory definition via the
/// pass-scoped [`SpAliasCfg`]; forwards iff that definition is an
/// exact-match `Store`.  Returns `Changed` iff the load's uses were rewired.
fn try_forward_load(
    ctx: &mut crate::EditFunction<'_>,
    load: NodeId,
    memo: &mut SpExprMemo,
    alias_mode: crate::AliasMode,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = ctx.graph_ref().node_inputs_exact::<2>(load)?;
    // A `Load` always produces a single value output (validated signature).
    let (load_value, load_ty) = ctx.single_value_output(load)?;

    let load_size = load_ty.byte_size() as i64;
    // load_forward stays conservative on distinct SP bases (a store at a
    // different SP base may still alias the forwarded load); a `Call` always
    // blocks a forward (`call_clobbers: true`).
    let mut alias_cfg = SpAliasCfg::call_blocking(memo, alias_mode);
    let load_class = alias_cfg.classify_addr(ctx.function(), addr);

    // 1. Find the nearest definition that may alias the load.  A clean
    //    chain returns the `InitialMemory` node (handled by the Store
    //    check below) → nothing to forward.  A `Call` always blocks a
    //    forward (`call_clobbers: true`).
    let clobber_node = alias_cfg.nearest_clobber(ctx, load, load_class, load_size, mem);

    // 2. The clobber must be a `Store`.  A `MemPhi` boundary (disagreeing
    //    control merge), a `Call` / `CallOther`, `InitialMemory` (clean
    //    chain), or any opaque producer is NOT forwardable.
    if !matches!(ctx.node_kind(clobber_node), NodeKind::Store(_)) {
        return Ok(OptimizationResult::NoChange);
    }

    // 3. Exact-match check: the store must write the SAME location
    //    (address class + base + offset) and its value must cover the
    //    load's byte range.  An overlapping-but-not-exact store bails.
    let store_addr = ctx.store_addr(clobber_node);
    let data = ctx.store_data(clobber_node);
    // A `Store`'s data input is an `AnyInt` value slot (validated), so its
    // source output is always a value.
    let data_ty = ctx
        .value_type_opt(data)
        .expect("Store data input is a value");
    let store_size = data_ty.byte_size() as i64;
    let store_class = alias_cfg.classify_addr(ctx.function(), store_addr);
    if alias_verdict(
        load_class,
        load_size,
        store_class,
        store_size,
        alias_mode,
        false,
    ) != AliasVerdict::Match
    {
        // Same-location offsets must coincide exactly; an
        // overlapping-but-shifted store is not forwardable.
        return Ok(OptimizationResult::NoChange);
    }

    // 4. Forward the stored value, reshaping a wider store down to the
    //    load width when needed.  These are value-reshaping nodes
    //    (`Truncate` / `ShiftRight`), never a `Phi`.
    let forwarded = if data_ty == load_ty {
        data
    } else if data_ty.is_integer()
        && load_ty.is_integer()
        && load_ty.byte_size() < data_ty.byte_size()
        // The BE reshape mints a shift const at `data_ty` via `build_int_const`,
        // which only materialises types up to I128 (I256/I512 route through the
        // wide interner separately).  Bail (NoChange) on a wider store rather
        // than fail the pass — such a wide-store→narrow-load forward is exotic.
        && data_ty.byte_size() <= 16
    {
        narrow(ctx, data, load)?
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
fn narrow(ctx: &mut crate::EditFunction<'_>, data: ValueId, load: NodeId) -> Result<ValueId> {
    // Both `data_ty` (the `Store` data input) and `load_ty` (the `Load`
    // output) are value-edge types, so derive them from the nodes the caller
    // already holds — each is an O(1) cached look-up — rather than threading
    // them in as redundant decomposed arguments.
    let data_ty = ctx.value_type(data)?;
    let (_, load_ty) = ctx.single_value_output(load)?;
    // SSoT: the byte order is the function's own.
    let endianness = ctx.function().endianness();
    let shifted = match endianness {
        Endianness::Little => data,
        Endianness::Big => {
            let shift_bits = ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
            // shift_bits is a byte-offset * 8 — always fits in u64.  Route it
            // through `build_int_const` so a wide `data_ty` (I80 / I128) mints
            // the interned const via `build_int_const` so it dedups correctly
            // against any other node with the same value and type.
            let shift_const = ctx.build_int_const(u128::from(shift_bits), data_ty)?;
            // `build_int_const` does not carry the `&[load]` contributor stamp
            // that `create_node_attributed` would, so attribute the fresh const
            // to the load explicitly — every reachable node must carry ≥1
            // asm-fingerprint.
            let shift_const_node = ctx.producer(shift_const);
            ctx.function_mut()
                .extend_asm_fingerprint_from(shift_const_node, load);
            let shr = ctx.create_node_attributed(
                NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
                [data, shift_const],
                [ValueKind::Typed(data_ty)],
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
