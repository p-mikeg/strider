//! Forwards the value of a `Store(addr=sp+K)` to a subsequent
//! `Load[sp + K]` when the live (nearest may-aliasing) memory definition
//! reaching the load is an **exact-match store** to the same location.
//!
//! The pass walks the memory-SSA chain backward from the load via the
//! shared [`crate::memory_ssa::may_clobber`] walker, with the shared
//! [`crate::sp_expr::SpAliasOracle`] (`call_clobbers: true` — a load
//! never forwards across a call) supplying the per-def aliasing verdict.
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
use strider_ir::node::{IntPayload, NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_target::Endianness;

use crate::error::Result;
use crate::memory_ssa::may_clobber;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{AliasVerdict, SpAliasOracle, SpExprMemo, alias_verdict, classify_addr};
use entity_utils::Worklist;

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
/// [`crate::OptCtx::alias_mode`] — the pass carries no configuration of its
/// own.
#[derive(Clone, Default)]
pub struct LoadForward;

impl LoadForward {
    /// Creates the pass.  The stack pointer and endianness come from the
    /// function under analysis; the alias precision comes from the shared
    /// [`crate::OptCtx::alias_mode`] at apply time.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Optimizer for LoadForward {
    fn apply(
        &self,
        ctx: &mut crate::EditFunction<'_>,
        opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let alias_mode = opt_ctx.options.alias_mode;
        let mut work: Worklist<NodeId> = ctx
            .reverse_postorder_filter(|k| matches!(k, NodeKind::Load(_)))
            .collect();
        // Local memo rather than `opt_ctx.sp_memo`: the post-passes
        // (`function_args` / `call_stack_args`) share `octx.sp_memo`, but
        // `LoadForward` runs inside the fixed-point loop and keeps a
        // per-`apply` memo — sharing would buy nothing since the post-passes
        // receive a freshly-cleared memo after the loop exits anyway.
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        while let Some(load) = work.dequeue() {
            result |= try_forward_load(ctx, load, &mut memo, alias_mode)?;
        }
        Ok(result)
    }
}

/// Tries to forward a single `Load` to the value of its live upstream
/// `Store`.  Finds the nearest may-aliasing memory definition via
/// [`may_clobber`] + the shared [`SpAliasOracle`]; forwards iff that
/// definition is an exact-match `Store`.  Returns `Changed` iff the
/// load's uses were rewired.
fn try_forward_load(
    ctx: &mut crate::EditFunction<'_>,
    load: NodeId,
    memo: &mut SpExprMemo,
    alias_mode: crate::AliasMode,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = ctx.graph_ref().node_inputs_exact::<2>(load)?;
    let [load_value] = ctx.node_outputs_exact::<1>(load)?;
    // A `Load` always produces a value output (validated signature).
    let load_ty = ctx
        .value_kind(load_value)
        .as_value()
        .expect("Load output is a value");

    let load_class = classify_addr(ctx.function(), addr, memo);
    let load_size = load_ty.byte_size() as i64;

    // 1. Find the nearest definition that may alias the load.  A clean
    //    chain returns the `InitialMemory` node (handled by the Store
    //    check below) → nothing to forward.  A `Call` always blocks a
    //    forward (`call_clobbers: true`).
    let clobber_node = {
        let mem_node = ctx.function().producer(mem);
        let mut oracle = SpAliasOracle {
            load_class,
            load_size,
            sp_memo: memo,
            alias_mode,
            call_clobbers: true,
            // load_forward stays conservative: a store at a different SP base
            // may still alias the forwarded load.
            distinct_sp_bases_disjoint: false,
        };
        may_clobber(ctx, &mut oracle, load, mem_node)
    };

    // 2. The clobber must be a `Store`.  A `MemPhi` boundary (disagreeing
    //    control merge), a `Call` / `CallOther`, `InitialMemory` (clean
    //    chain), or any opaque producer is NOT forwardable.
    if !matches!(ctx.node_kind(clobber_node), NodeKind::Store(_)) {
        return Ok(OptimizationResult::NoChange);
    }

    // 3. Exact-match check: the store must write the SAME location
    //    (address class + base + offset) and its value must cover the
    //    load's byte range.  An overlapping-but-not-exact store bails.
    // Store inputs: [memory, addr, data] — exactly 3 once the kind is
    // established (validated structural invariant).
    let [_store_mem, store_addr, data] = ctx.graph_ref().node_inputs_exact::<3>(clobber_node)?;
    // A `Store`'s data input is an `AnyInt` value slot (validated), so its
    // source output is always a value.
    let data_ty = ctx
        .value_kind(data)
        .as_value()
        .expect("Store data input is a value");
    let store_size = data_ty.byte_size() as i64;
    let store_class = classify_addr(ctx.function(), store_addr, memo);
    if alias_verdict(load_class, load_size, store_class, store_size, alias_mode, false)
        != AliasVerdict::Match
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
    {
        narrow(ctx, data, data_ty, load_ty, load)?
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
fn narrow(
    ctx: &mut crate::EditFunction<'_>,
    data: ValueId,
    data_ty: ValueType,
    load_ty: ValueType,
    load: NodeId,
) -> Result<ValueId> {
    // SSoT: the byte order is the function's own.
    let endianness = ctx.function().endianness();
    let shifted = match endianness {
        Endianness::Little => data,
        Endianness::Big => {
            let shift_bits = ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
            // shift_bits is a byte-offset * 8 — always fits in u64 for ≤I64.
            let shift_const_node = ctx.create_node_attributed(
                NodeKind::IntConst(IntPayload::Small(
                    (u128::from(shift_bits) & data_ty.bit_mask_u128()) as u64,
                )),
                [],
                [ValueKind::Typed(data_ty)],
                &[load],
            );
            let [shift_const] = ctx.node_outputs_exact::<1>(shift_const_node)?;
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
