//! Forwards the value of a `Store(addr=sp+K)` to a subsequent
//! `Load[sp + K]` when the live (nearest may-aliasing) memory definition
//! reaching the load is an **exact-match store** to the same location.
//!
//! The pass walks the memory-SSA chain backward from the load via the
//! shared [`crate::opt::memory_ssa::may_clobber`] walker, with a
//! [`LoadForwardOracle`] supplying the per-def aliasing verdict.  The
//! walker returns the nearest may-aliasing definition NODE:
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
//! Must be wired into the pipeline with the calling convention's
//! stack-pointer varnode and the target's endianness (see
//! [`LoadForward::new`]).

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_target::Endianness;

use crate::opt::OptRewrite;
use crate::opt::error::Result;
use crate::opt::memory_ssa::{MemorySSAWalker, may_clobber};
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{
    alias_verdict, classify_addr, store_alias_verdict, AddrClass, AliasVerdict, SpExpr,
    SpExprMemo, decompose_sp, ranges_disjoint,
};
use crate::opt::worklist::seeded_kind;

/// Store-to-load forwarding for SP-relative stack slots.
///
/// Runs inside the main fixed-point loop so that stack stores classified by
/// `StackOffsetDetect` become visible to the walker on subsequent iterations,
/// and so that forwarded constants fed into expressions are in turn
/// simplified by `ConstantFold` / `KnownBits`.
#[derive(Clone)]
pub struct LoadForward {
    /// Stack-pointer varnode used by [`decompose_sp`] to recognise
    /// SP-relative addresses.  Extracted from the calling convention at
    /// construction time — the pass consults nothing else from the CC.
    stack_vn: rsleigh::Vn,
    /// Target endianness — controls how a narrow load from a wider store is
    /// synthesised (LE: low bytes via `Truncate`; BE: high bytes via
    /// `Truncate(ShiftRight(data, (store_size - load_size) * 8))`).
    ///
    /// Carried separately from the CC because endianness is a
    /// per-arch property (lives on [`strider_target::SleighArch`])
    /// rather than a per-CC property.
    endianness: Endianness,
    /// Alias-analysis precision for the backward chain walk.  Default
    /// is [`crate::opt::AliasMode::AssumeStackGlobalDisjoint`].
    alias_mode: crate::opt::AliasMode,
}

impl LoadForward {
    /// Creates a new pass for the given stack-pointer varnode and target
    /// endianness.  Convenience constructor; production paths prefer
    /// [`Self::from_convention`] so the same CC is shared with the
    /// other SP-aware passes.
    #[must_use]
    pub const fn new(stack_vn: rsleigh::Vn, endianness: Endianness) -> Self {
        Self {
            stack_vn,
            endianness,
            alias_mode: crate::opt::AliasMode::AssumeStackGlobalDisjoint,
        }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from `cc` and
    /// whose endianness is taken from `arch`.
    #[must_use]
    pub fn from_convention(
        cc: &strider_target::BuiltCallingConvention,
        arch: &strider_target::SleighArch,
    ) -> Self {
        Self::new(cc.stack_vn, arch.endianness())
    }

    /// Overrides the alias-analysis precision used by the chain walk.
    /// See [`crate::opt::AliasMode`] for the soundness/coverage trade-off.
    #[must_use]
    pub const fn alias_mode(mut self, mode: crate::opt::AliasMode) -> Self {
        self.alias_mode = mode;
        self
    }
}

impl Optimizer for LoadForward {
    fn apply(
        &self,
        ctx: &mut strider_pattern::RewriteCtx<'_>,
        _opt_ctx: &crate::opt::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        let stack_vn = self.stack_vn;
        while let Some(load) = work.dequeue() {
            result |= try_forward_load(ctx, load, stack_vn, self.endianness, &mut memo, self.alias_mode)?;
        }
        Ok(result)
    }
}

/// Tries to forward a single `Load` to the value of its live upstream
/// `Store`.  Finds the nearest may-aliasing memory definition via
/// [`may_clobber`] + [`LoadForwardOracle`]; forwards iff that
/// definition is an exact-match `Store`.  Returns `Changed` iff the
/// load's uses were rewired.
fn try_forward_load(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    load: NodeId,
    stack_vn: rsleigh::Vn,
    endianness: Endianness,
    memo: &mut SpExprMemo,
    alias_mode: crate::opt::AliasMode,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = ctx.graph_ref().node_inputs_exact::<2>(load)?;
    let [load_value] = ctx.node_outputs_exact::<1>(load)?;
    // A `Load` always produces a value output (validated signature).
    let load_ty = ctx
        .value_kind(load_value)
        .as_value()
        .expect("Load output is a value");

    let load_class = classify_addr(ctx.function_ref(), addr, stack_vn, memo);
    let load_size = load_ty.byte_size() as i64;

    // 1. Find the nearest definition that may alias the load.  A clean
    //    chain returns the `InitialMemory` node (handled by the Store
    //    check below) → nothing to forward.
    let clobber_node = {
        let mut oracle = LoadForwardOracle {
            load_class,
            load_size,
            stack_vn,
            memo,
            alias_mode,
        };
        let mem_node = ctx.function_ref().producer(mem);
        may_clobber(ctx.function_ref(), &mut oracle, load, mem_node)
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
    let store_class = classify_addr(ctx.function_ref(), store_addr, stack_vn, memo);
    if alias_verdict(load_class, load_size, store_class, store_size, alias_mode)
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
        narrow(ctx, data, data_ty, load_ty, endianness, load)?
    } else {
        // Same offset but the stored bytes do not fully back the load
        // (narrower store, or a non-integer reshape) → cannot forward.
        return Ok(OptimizationResult::NoChange);
    };

    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // forwarded producer and redirects all uses.  The reshaping nodes built
    // in `narrow` are each attributed via `create_node_attributed(..,
    // &[load])`, so the contract holds at every intermediate node.
    let changed = ctx.replace_value(load_value, forwarded)?;
    if changed {
        ctx.detach_node_inputs(load);
    }
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
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    data: ValueId,
    data_ty: ValueType,
    load_ty: ValueType,
    endianness: Endianness,
    load: NodeId,
) -> Result<ValueId> {
    let shifted = match endianness {
        Endianness::Little => data,
        Endianness::Big => {
            let shift_bits = ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
            let shift_const_node = ctx.create_node_attributed(
                NodeKind::IntConst(u128::from(shift_bits) & data_ty.bit_mask_u128()),
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

/// [`MemorySSAWalker`] oracle for the store-to-load forwarder.
///
/// `def_clobbers` answers "does this memory def overlap the load's byte
/// range?":
///
/// * `Store` — classified via [`alias_verdict`] against the load's
///   address class + size: `Match` or `MayAlias` → overlaps (`true`);
///   `Disjoint` → steps through (`false`).  The walker then returns the
///   nearest overlapping store; the caller re-checks exact-`Match` before
///   forwarding.
/// * `Call` / `CallOther` — clobbers memory, so it overlaps any load
///   (`true`); a load whose live def is a call does not forward.
/// * any other (opaque) memory producer — conservatively overlaps
///   (`true`).
///
/// `MemPhi` is handled structurally by [`may_clobber`] (agree →
/// pass through, disagree → the phi is the boundary), so the oracle never
/// sees one.
struct LoadForwardOracle<'a> {
    load_class: AddrClass,
    load_size: i64,
    stack_vn: rsleigh::Vn,
    memo: &'a mut SpExprMemo,
    alias_mode: crate::opt::AliasMode,
}

impl<'a> MemorySSAWalker for LoadForwardOracle<'a> {
    fn def_clobbers(
        &mut self,
        function: &strider_ir::Function,
        _load: NodeId,
        def: NodeId,
    ) -> bool {
        match *function.node_kind(def) {
            // A store is a clobber unless provably `Disjoint`.  Both
            // `Match` (the forwarding source) and `MayAlias` terminate the
            // walk here; the caller re-checks exact-`Match` before
            // forwarding.
            NodeKind::Store(_) => store_alias_verdict(
                function,
                def,
                self.load_class,
                self.load_size,
                self.stack_vn,
                self.memo,
                self.alias_mode,
            ) != AliasVerdict::Disjoint,
            // Every other memory producer is a clobber here: a `Call` /
            // `CallOther` clobbers memory and overlaps any load, and any
            // opaque producer cannot be proven disjoint — both terminate
            // the walk so the load does not forward across them.
            _ => true,
        }
    }
}


// ── Public helper for the indirect-branch classifier ──────
//
// `try_forward_load` rewrites the load by bottoming-out the memory chain at
// a stack-tagged `Store` and re-using its data slot.  When the load address has a
// concrete SP-relative offset, that's straightforward.  But the
// computed-goto-via-stack-array shape has a *symbolic* offset
// (`sp + base + idx*stride`) — the per-i target lives at offset
// `base + i*stride` for i in [0, N), bounded by KnownBits.
//
// The indirect-branch classifier needs to enumerate per-i values without rewriting
// the load (no IR primitive expresses "value depends on idx" without a
// `Region` for an anonymous `Phi` to bind to).  This helper exposes the
// stack-tagged-`Store`-chain walk as a pub function: given a memory chain root
// and a concrete offset, return the `ValueId` of the value stored
// there (or `None` when the chain has no matching store, has an aliasing
// intermediate, or terminates at `InitialMemory`).
//
// SOUNDNESS — restricted to the no-MemPhi case (the classifier asks one
// concrete offset at a time):
//   * stack-tagged `Store { offset == requested }` with matching value type:
//     return the stored `data` output.  This is sound because no later
//     write can have aliased the slot — we walked here from the load's
//     memory input through strictly-earlier stores, and the offset
//     equality check is exact (StackOffsetDetect tagged it).
//   * stack-tagged `Store` at a different offset: skip iff the byte ranges are
//     provably disjoint (`ranges_disjoint`); recurse on the prior
//     memory.
//   * `Store(_)` (raw, untagged): probe its address.  If it's
//     not SP-rooted (`decompose_sp` returns `None`), it cannot alias
//     a stack slot; recurse.  If it IS SP-rooted (a terminal `sp + k`),
//     recurse iff disjoint.  (This helper is single-region, where the
//     stack pointer never joins through a multi-predecessor phi, so a
//     non-decomposable SP-phi address does not arise.)
//   * `MemPhi`: cross-region join.  This helper does NOT recurse
//     across MemPhi (returns `None`) — the case is single-
//     region (the prologue stores and the dispatch load live in the
//     same region) and the classifier asks one offset at a time.
//   * `InitialMemory` / anything else: return `None`.
//
// Type strictness: the helper returns `None` if the stack-tagged Store's value
// type doesn't equal `value_type` exactly.  Narrow-load-from-wider-store
// is intentionally NOT implemented here — the classifier only consumes
// IntConst targets, and a Truncate(IntConst) folds to IntConst via
// ConstantFold, so the narrow case shows up as a wide-typed IntConst-valued
// store that the classifier can read directly.

/// Per-call memo for `find_stack_stored_value_at_offset`, keyed on
/// `(memory_token, offset, value_type)`.  Threaded through the
/// indirect-branch classifier loops so repeated lookups across
/// enumerated jump-table indices share their walks.
pub type StackStoredValueMemo =
    rustc_hash::FxHashMap<(ValueId, i64, ValueType), Option<ValueId>>;

/// Walks the memory chain backward from `mem` looking for a
/// `Store(addr=sp+offset)` whose stored value has type `value_type`.
/// Returns the stored value's output id on success, or `None` when no
/// matching store dominates the chain.
///
/// See the module-level "Public helper for the indirect-branch
/// classifier" notes for the soundness rules.
///
/// # Permissiveness (do not rely on this for cross-base disjointness)
///
/// This is a deliberately permissive stack-slot lookup written for the
/// indirect-branch stack-array classifier, and it is *more* permissive
/// than the shared `crate::opt::sp_expr::walk` step:
///
/// - **Walks past non-SP-rooted stores unconditionally.**  When the
///   store's address does not decompose to an SP expression (the `None`
///   arm), it skips the store and continues down `inputs[0]`, with no
///   `AliasMode` gate and accepting opaque pointer addresses — assuming
///   stack and non-stack memory are disjoint.
/// - **Keys slots by offset only, not by base.**  The
///   `SpExpr { base: _, offset: k }` arm matches on `k == offset`
///   alone and ignores the SP `base`, so two distinct SP-relative bases
///   that share an offset are treated as the same slot.
///
/// Both are sound for the single-frame jump-table-array use this helper
/// serves, but callers MUST NOT rely on it for cross-base disjointness.
///
/// # Parameters
///
/// - `function` — the IR function to walk (read-only).
/// - `mem` — the chain root (typically a Load's memory-input slot).
/// - `offset` — the SP-relative offset of the requested slot.
/// - `value_type` — the expected stored value's type.  Mismatched
///   types return `None` (no Truncate / ShiftRight synthesis here).
/// - `stack_vn` — the calling convention's stack-pointer varnode.
/// - `sp_memo` — a per-call SP-decomposition memo.
/// - `walk_memo` — a per-call result memo keyed on `(mem, offset,
///   value_type)`.
#[must_use]
pub(crate) fn find_stack_stored_value_at_offset(
    function: &strider_ir::Function,
    mem: ValueId,
    offset: i64,
    value_type: ValueType,
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    walk_memo: &mut StackStoredValueMemo,
) -> Option<ValueId> {
    // Iterative form (was recursive; deep prologues blew the stack).
    // Walks the memory-chain backward via the Store's inputs[0].
    // Stack-safe at any chain depth.
    let load_size = value_type.byte_size() as i64;
    let mut visited: Vec<(ValueId, i64, ValueType)> = Vec::new();
    let mut cur_mem = mem;

    let result: Option<ValueId> = loop {
        let key = (cur_mem, offset, value_type);
        if let Some(&cached) = walk_memo.get(&key) {
            break cached;
        }
        visited.push(key);
        let node = function.producer(cur_mem);
        match *function.node_kind(node) {
            NodeKind::Store(_) => {
                // Store inputs: [memory, addr, data] — exactly 3 once the
                // kind is established (validated structural invariant).
                let inputs = function.graph().node_inputs_exact::<3>(node)
                    .expect("Store node has 3 inputs (validated)");
                let addr = inputs[1];
                let data = inputs[2];
                match decompose_sp(function, addr, stack_vn, sp_memo) {
                    Some(SpExpr { base: _, offset: k }) => {
                        // A `Store`'s data input is an `AnyInt` value slot
                        // (validated), so its source output is always a value.
                        let data_ty = function
                            .value_kind(data)
                            .as_value()
                            .expect("Store data input is a value");
                        if k == offset {
                            if data_ty == value_type {
                                break Some(data);
                            }
                            break None;
                        } else {
                            let store_size = data_ty.byte_size() as i64;
                            if ranges_disjoint(k, store_size, offset, load_size) {
                                cur_mem = inputs[0];
                                continue;
                            }
                            break None;
                        }
                    }
                    None => {
                        cur_mem = inputs[0];
                        continue;
                    }
                }
            }
            // MemPhi / InitialMemory / anything else: bail.  See module
            // notes for why MemPhi handling is intentionally future work.
            _ => break None,
        }
    };

    // Memoise every prefix on the way back so future queries reuse work.
    for key in visited {
        walk_memo.insert(key, result);
    }
    result
}

#[cfg(test)]
mod tests;
