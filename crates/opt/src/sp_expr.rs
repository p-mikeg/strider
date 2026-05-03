//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`stack_store::detect`, `stack_load_forward`, `function_args::stack_args`).
//!
//! `decompose_sp` is the workhorse: given an output that may be `InitialVar(sp)`
//! transformed by `Add`/`Sub` of constants and joined by `VarPhi(sp)`, it
//! returns either a `Terminal { base, offset }` or a `Phi { node, offsets[] }`.
//! Callers thread a per-pass-call memo through it so repeated walks over the
//! same SP chain cost O(1) on cache hit.
//!
//! Invariant: `Phi { offsets[j] }` requires every predecessor `j` to itself
//! decompose to a `Terminal { base: InitialVar(sp), offset }`. A predecessor
//! that fails to decompose, or that decomposes to a nested `Phi`, makes the
//! whole walk return `None` rather than fabricate a Terminal — callers depend
//! on `offset` being literally correct (e.g. on conventions where
//! `stack_arg_offsets[0] == 0`, a fabricated `offset = 0` would be silently
//! misclassified as the first stack argument).

use rustc_hash::{FxHashMap, FxHashSet};

use ir::node::{NodeId, NodeKind, NodeOutputId};
use ir::{Graph, IntBinaryOp};

/// Decomposed stack-pointer expression.
///
/// `pub` so out-of-crate callers (e.g. the tier-2 indirect-branch classifier
/// in `crates/strider`) can drive [`decompose_sp`] when matching the 
/// `Load[sp + base + idx*stride]` shape.
#[derive(Clone, Debug)]
pub enum SpExpr {
    /// `base + offset`, where `base` is an SP-rooted node.
    Terminal { base: NodeOutputId, offset: i64 },
    /// `VarPhi(stack_ptr)` where every predecessor resolves to
    /// `InitialVar(stack_ptr) + offsets[j]`.
    Phi { phi_node: NodeId, offsets: Vec<i64> },
}

impl SpExpr {
    #[must_use]
    pub(crate) fn shifted(self, delta: i64) -> Self {
        match self {
            SpExpr::Terminal { base, offset } => SpExpr::Terminal {
                base,
                offset: offset.wrapping_add(delta),
            },
            SpExpr::Phi { phi_node, offsets } => SpExpr::Phi {
                phi_node,
                offsets: offsets.into_iter().map(|o| o.wrapping_add(delta)).collect(),
            },
        }
    }
}

/// True when `[a_off, a_off + a_size)` and `[b_off, b_off + b_size)` are
/// disjoint.
///
/// Endpoint computations use `saturating_add` so that callers passing
/// `size = i64::MAX` as a soundness-pessimistic fallback (e.g. when a Store's
/// `value_byte_size` is unknown) cannot panic in debug or wrap in release.
/// A saturated upper endpoint additionally short-circuits to "not disjoint"
/// — i.e. an unknown-extent range is treated as effectively infinite in both
/// directions, matching the conservative verdict callers expect.
#[inline]
#[must_use]
pub fn ranges_disjoint(a_off: i64, a_size: i64, b_off: i64, b_size: i64) -> bool {
    let a_end = a_off.saturating_add(a_size);
    let b_end = b_off.saturating_add(b_size);
    // If either endpoint saturated, treat the corresponding range as
    // unbounded and report "not disjoint" — the conservative answer.
    if a_end == i64::MAX || b_end == i64::MAX {
        return false;
    }
    a_end <= b_off || b_end <= a_off
}

/// Conservative byte size of a `Store`'s DATA slot, used as a range bound
/// for [`ranges_disjoint`].  Returns the value type's byte size when the
/// slot is value-typed (the IR signature guarantees this for any valid
/// `Store`); otherwise returns `i64::MAX` so callers' `ranges_disjoint`
/// checks fail closed (treat the unknown extent as effectively infinite,
/// the soundness-preserving verdict).
///
/// The fallback branch is unreachable in valid IR but exists as a
/// defensive guardrail — its rationale is duplicated across every caller
/// otherwise, so it lives here.
#[inline]
#[must_use]
pub(crate) fn store_value_byte_size(g: &Graph, store_data: NodeOutputId) -> i64 {
    g.output_kind(store_data)
        .as_value()
        .map_or(i64::MAX, |t| t.byte_size() as i64)
}

/// Outcome of inspecting a memory-chain node for the byte range
/// `[query_off, query_off + query_size)`: either the node may alias and
/// further walking must terminate, or the prior memory output is safe to
/// recurse on.
pub(crate) enum AliasStep {
    /// The node is provably non-aliasing with the query range — walk to
    /// `prev_mem` to keep searching.
    PassThrough { prev_mem: NodeOutputId },
    /// The node may alias the query range (overlapping byte ranges, an
    /// SP-rooted Phi address, or malformed inputs).  Caller must terminate.
    MayAlias,
}

/// Decides whether walking past `node` (a `NodeKind::StackStore`) is safe
/// for a search over `[query_off, query_off + query_size)`.
pub(crate) fn step_through_stack_store(
    graph: &Graph,
    node: NodeId,
    store_offset: i64,
    query_off: i64,
    query_size: i64,
) -> AliasStep {
    // StackStore inputs: [MEM, SP, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    let store_size = store_value_byte_size(graph, inputs[2]);
    if ranges_disjoint(store_offset, store_size, query_off, query_size) {
        AliasStep::PassThrough { prev_mem: inputs[0] }
    } else {
        AliasStep::MayAlias
    }
}

/// Decides whether walking past `node` (a `NodeKind::StackStorePhi`) is
/// safe.  The phi disqualifies if any per-predecessor offset (stored in
/// `Graph::stack_phi_offsets`) overlaps the query range.
pub(crate) fn step_through_stack_store_phi(
    graph: &Graph,
    node: NodeId,
    query_off: i64,
    query_size: i64,
) -> AliasStep {
    // StackStorePhi inputs: [PHI, MEM, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    let store_size = store_value_byte_size(graph, inputs[2]);
    let any_overlap = graph
        .stack_phi_offsets(node)
        .iter()
        .any(|&k| !ranges_disjoint(k, store_size, query_off, query_size));
    if any_overlap {
        AliasStep::MayAlias
    } else {
        AliasStep::PassThrough { prev_mem: inputs[1] }
    }
}

/// Decides whether walking past `node` (a raw `NodeKind::Store`) is safe.
/// Decomposes the store address: a non-SP-rooted address is provably
/// non-aliasing with the SP-relative query range; an SP-rooted Terminal
/// address uses the same disjointness check; an SP-rooted Phi address
/// conservatively terminates.
pub(crate) fn step_through_store(
    graph: &Graph,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    query_off: i64,
    query_size: i64,
) -> AliasStep {
    // Store inputs: [MEM, ADDR, DATA].
    let inputs = graph.node_inputs(node);
    if inputs.len() < 3 {
        return AliasStep::MayAlias;
    }
    let mut sp_visiting = FxHashSet::default();
    match decompose_sp(graph, inputs[1], sp_vn, sp_memo, &mut sp_visiting) {
        // Non-SP-rooted address provably cannot alias the stack-arg byte
        // range — walk through.
        None => AliasStep::PassThrough { prev_mem: inputs[0] },
        Some(SpExpr::Terminal { base: _, offset: store_off }) => {
            let store_size = store_value_byte_size(graph, inputs[2]);
            if ranges_disjoint(store_off, store_size, query_off, query_size) {
                AliasStep::PassThrough { prev_mem: inputs[0] }
            } else {
                AliasStep::MayAlias
            }
        }
        // SP-rooted Phi: per-predecessor range analysis would be needed to
        // prove disjointness; conservatively terminate.
        Some(SpExpr::Phi { .. }) => AliasStep::MayAlias,
    }
}

/// Reads an integer-constant output as signed, sign-extended from its declared
/// bit width. Returns `None` for non-integer-constant or when the
/// sign-extended value does not fit in `i64`.
///
/// Also recognises `IntUnaryOp::Neg(IntConst(K))` as the signed value `-K`
/// so callers can treat the lowered subtraction shape
/// (`Add(_, Neg(IntConst(K)))`) the same way they treat the post-`ConstantFold`
/// shape (`Add(_, IntConst(-K))`).  Without this peephole the SP-expression
/// walker would return `None` during fixed-point iterations where
/// `ConstantFold` hasn't yet collapsed the `Neg` of a constant, breaking
/// `StackStoreDetect`'s ability to make progress on the same iteration.
#[must_use]
pub(crate) fn int_const_signed(g: &Graph, out: NodeOutputId) -> Option<i64> {
    if let Some(c) = g.int_const_val(out) {
        let signed = g.output_kind(out).as_value()?.get_signed_int(u128::from(c))?;
        return i64::try_from(signed).ok();
    }
    // Peephole: Neg(IntConst(K)) → wrapping-negate K modulo the inner
    // type's width, then sign-extend.  The lifter produces this shape for
    // every `IntSub _, IntConst(K)`; intermediate fixed-point iterations
    // may inspect the graph before `ConstantFold` collapses the
    // `Neg(IntConst)` to a single negative `IntConst`.
    //
    // Use modular `wrapping_neg` (matching `ConstantFold`'s
    // `IntUnaryOp::Neg` evaluator at `constant_fold/rules.rs:413`) rather
    // than `checked_neg` on the sign-extended value.  Without modular
    // negation we would silently disagree with `ConstantFold` on the
    // type-minimum input (e.g. `0x80000000_U32`): `checked_neg` on the
    // sign-extended `-2^31` yields `+2^31`, while the IR's actual
    // semantics (modular two's-complement) yield `0x80000000_U32` which
    // sign-extends to `-2^31`.  The pre-fold and post-fold view of the
    // same SP-relative subtraction would then return different offsets
    // and `StackStoreDetect` could classify the same store inconsistently.
    let node = g.get_node_from_output(out);
    if matches!(g.node_kind(node), NodeKind::IntUnaryOp(ir::IntUnaryOp::Neg)) {
        let inputs = g.node_inputs(node);
        if inputs.len() == 1 {
            let inner = inputs[0];
            let k = g.int_const_val(inner)?;
            let inner_ty = g.output_kind(inner).as_value()?;
            let neg_raw = u128::from(k).wrapping_neg();
            let neg_masked = inner_ty.get_unsigned_int(neg_raw)?;
            let signed = inner_ty.get_signed_int(neg_masked)?;
            return i64::try_from(signed).ok();
        }
    }
    None
}

/// Per-pass-call memo for `decompose_sp`.
pub type SpExprMemo = FxHashMap<NodeOutputId, Option<SpExpr>>;

/// Decomposes `out` into `InitialVar(sp) + K` (or per-branch equivalent),
/// caching definitive results in `memo`. The `visiting` set guards against
/// cycles through `VarPhi` back-edges; cycle-broken results are NOT
/// memoized (so a different call path can still resolve the same output).
pub fn decompose_sp(
    g: &Graph,
    out: NodeOutputId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut FxHashSet<NodeId>,
) -> Option<SpExpr> {
    if let Some(cached) = memo.get(&out) {
        return cached.clone();
    }
    let node = g.get_node_from_output(out);
    if !visiting.insert(node) {
        // Cycle: do NOT cache (a different call path may resolve it).
        return None;
    }
    let result = decompose_sp_inner(g, out, node, sp_vn, memo, visiting);
    visiting.remove(&node);
    // Cache `Some(_)` results unconditionally — the decomposition is a
    // deterministic function of `out`. Don't cache `None`: it could mean
    // "genuinely not SP-rooted" (safe to recompute) OR "cycle-truncated on
    // this call path" (must NOT be cached, since a different call path
    // where `node` isn't on the stack may decompose it cleanly). The
    // `Some(_)` filter is sound because the cycle-truncation early-return
    // above always returns `None`.
    if let Some(ref e) = result {
        memo.insert(out, Some(e.clone()));
    }
    result
}

fn decompose_sp_inner(
    g: &Graph,
    out: NodeOutputId,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut FxHashSet<NodeId>,
) -> Option<SpExpr> {
    match *g.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == sp_vn => Some(SpExpr::Terminal {
            base: out,
            offset: 0,
        }),
        NodeKind::VarPhi(vn) if vn == sp_vn => {
            decompose_sp_phi(g, node, sp_vn, memo, visiting)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            let inputs = g.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let l = inputs[0];
            let r = inputs[1];
            if let Some(c) = int_const_signed(g, r) {
                decompose_sp(g, l, sp_vn, memo, visiting).map(|e| e.shifted(c))
            } else if let Some(c) = int_const_signed(g, l) {
                decompose_sp(g, r, sp_vn, memo, visiting).map(|e| e.shifted(c))
            } else {
                None
            }
        }
        // x86 cdecl alignment dance: `and $0xfffffff8, %esp` (or wider
        // `0xfffffff0` for SSE-aligned frames).  The And's output is
        // runtime-aligned `(SP & mask)` — its exact value depends on the
        // entry SP's alignment, so the offset relative to `InitialVar(sp)`
        // is unknown.  But within the function the And's output is *fixed*
        // and serves as a stable opaque base for every subsequent stack
        // address.  Return `Terminal { base: <And output>, offset: 0 }`
        // so downstream Adds / Subs of constants chain through normally
        // and `StackStoreDetect` can rewrite the post-alignment stores
        // into `StackStore`s sharing this base.  Subsequent walkers
        // (`CallStackArgCollect`, `StackLoadForward`) compare offsets
        // relative to the matched base, so every aligned-frame store is
        // mutually comparable.
        //
        // Only matches when the non-mask operand is itself an SP-rooted
        // expression — guards against `And(rax, mask)` accidentally
        // producing a fake stack base.
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let inputs = g.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let l = inputs[0];
            let r = inputs[1];
            let sp_input = if int_const_signed(g, r).is_some() {
                l
            } else if int_const_signed(g, l).is_some() {
                r
            } else {
                return None;
            };
            decompose_sp(g, sp_input, sp_vn, memo, visiting).map(|_| SpExpr::Terminal {
                base: out,
                offset: 0,
            })
        }
        _ => None,
    }
}

fn decompose_sp_phi(
    g: &Graph,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut FxHashSet<NodeId>,
) -> Option<SpExpr> {
    let inputs = g.node_inputs(node);
    // A VarPhi has inputs[0] = dispatch token, inputs[1..] = per-pred
    // values. Fewer than 2 inputs means no actual predecessor — the phi is
    // either malformed or has been simplified mid-pass; we cannot prove
    // SP-rooted, so return None rather than fabricate a Terminal that lies
    // about base/offset.
    if inputs.len() < 2 {
        return None;
    }
    let mut offsets = Vec::with_capacity(inputs.len() - 1);
    let mut bases = Vec::with_capacity(inputs.len() - 1);
    for pred_input in inputs.into_iter().skip(1) {
        // If any predecessor is not a Terminal SP-rooted expression we
        // cannot describe this phi as InitialVar(sp) + K on every branch.
        // Fail closed (None) — callers' lookups against `stack_arg_offsets`
        // depend on `offset` being correct, and on conventions where
        // stack_arg_offsets[0] == 0 a fabricated `offset = 0` would be
        // silently misclassified as the first stack arg.
        let SpExpr::Terminal { base, offset } =
            decompose_sp(g, pred_input, sp_vn, memo, visiting)?
        else {
            return None;
        };
        bases.push(base);
        offsets.push(offset);
    }
    if bases.iter().all(|&b| b == bases[0]) && offsets.iter().all(|&o| o == offsets[0]) {
        Some(SpExpr::Terminal { base: bases[0], offset: offsets[0] })
    } else {
        Some(SpExpr::Phi { phi_node: node, offsets })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ir::node::NodeOutputType;
    use ir::{FunctionBuilder, IntBinaryOp};

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    #[test]
    fn ranges_disjoint_basic() {
        // Adjacent ranges are disjoint (touching is fine).
        assert!(ranges_disjoint(0, 4, 4, 4));
        // Overlapping ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 2, 4));
        // Identical ranges are not disjoint.
        assert!(!ranges_disjoint(0, 4, 0, 4));
        // Reverse order — equally disjoint.
        assert!(ranges_disjoint(4, 4, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_left_does_not_panic_and_is_conservative() {
        // The three memory-chain walkers (CallStackArgCollect,
        // stack_load_forward::probe, function_args::mem_chain_is_dirty)
        // pass `i64::MAX` as a soundness-pessimistic fallback when a Store's
        // `value_byte_size` is unknown. With plain `+`, `a_off + i64::MAX`
        // would panic in debug and wrap in release for any positive `a_off`.
        // ranges_disjoint must saturate cleanly and report "not disjoint"
        // (false) for any reachable load offset — the conservative verdict
        // callers depend on. SP-relative offsets in practice are small (kB
        // range), so we cover zero, modestly-negative, and modestly-positive
        // a_off values.
        assert!(!ranges_disjoint(0, i64::MAX, 100, 4));
        assert!(!ranges_disjoint(-1000, i64::MAX, 100, 4));
        assert!(!ranges_disjoint(1_000_000, i64::MAX, -1_000_000, 4));
        // Even very large positive a_off (where `a_off + i64::MAX` would
        // overflow without saturation) must not panic and must report
        // "not disjoint".
        assert!(!ranges_disjoint(1, i64::MAX, 0, 4));
    }

    #[test]
    fn ranges_disjoint_max_size_right_does_not_panic_and_is_conservative() {
        // Symmetric: i64::MAX on the b-side must also saturate and report
        // "not disjoint" without panicking.
        assert!(!ranges_disjoint(100, 4, 0, i64::MAX));
        assert!(!ranges_disjoint(100, 4, -1000, i64::MAX));
        assert!(!ranges_disjoint(-1_000_000, 4, 1_000_000, i64::MAX));
        assert!(!ranges_disjoint(0, 4, 1, i64::MAX));
    }

    #[test]
    fn int_const_signed_u32_negative() -> crate::Result<()> {
        // 0xFFFF_FFFC at U32 must read as -4 signed.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let v = b.build_int_const(0xFFFF_FFFCu64, NodeOutputType::U32)?;
        b.build_return(Some(v), &[])?;
        let fg = b.build()?;
        assert_eq!(int_const_signed(&fg.graph, v), Some(-4));
        Ok(())
    }

    #[test]
    fn int_const_signed_neg_of_min_uses_modular_negation() -> crate::Result<()> {
        // `Neg(IntConst(0x8000_0000_U32))` must agree with the IR's modular
        // semantics: in two's-complement at U32, `-(-2^31) = -2^31`.  The
        // peephole here must NOT return `+2^31` (which is what
        // `checked_neg(-2^31i128)` produces) — that would silently disagree
        // with `ConstantFold::IntUnaryOp::Neg`'s `wrapping_neg` evaluator
        // and `StackStoreDetect` could classify the same store inconsistently
        // depending on whether the inner Neg had been folded yet.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let inner = b.build_int_const(0x8000_0000u64, NodeOutputType::U32)?;
        let neg = b.build_int_unary_operation(inner, ir::IntUnaryOp::Neg, NodeOutputType::U32)?;
        b.build_return(Some(neg), &[])?;
        let fg = b.build()?;
        // Modular: wrapping_neg(0x8000_0000) = 0x8000_0000 → sign-extended to i32 = -2^31.
        assert_eq!(int_const_signed(&fg.graph, neg), Some(i32::MIN.into()));
        Ok(())
    }

    #[test]
    fn int_const_signed_neg_of_positive_const() -> crate::Result<()> {
        // Sanity: `Neg(IntConst(7_U32))` peeps through to `-7`.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let inner = b.build_int_const(7u64, NodeOutputType::U32)?;
        let neg = b.build_int_unary_operation(inner, ir::IntUnaryOp::Neg, NodeOutputType::U32)?;
        b.build_return(Some(neg), &[])?;
        let fg = b.build()?;
        assert_eq!(int_const_signed(&fg.graph, neg), Some(-7));
        Ok(())
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::Result<()> {
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        b.build_return(Some(sp_val), &[])?;
        let fg = b.build()?;
        // sp_val is a VarPhi-of-InitialVar; the phi has 1 predecessor →
        // collapses to Terminal{base: InitialVar(sp), offset: 0}.
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, sp_val, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: 0, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::Result<()> {
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, addr, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::Result<()> {
        // Add(sp, 0xFFFF_FFFC_U32) must decompose to -4 (sign-extended).
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let neg_four = b.build_int_const(0xFFFF_FFFCu64, NodeOutputType::U32)?;
        let addr = b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, addr, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_hit_returns_same_result() -> crate::Result<()> {
        // Calling decompose_sp twice on the same out should populate the memo
        // and return the same answer.
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr = b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r1 = {
            let mut v = FxHashSet::default();
            decompose_sp(&fg.graph, addr, sp, &mut memo, &mut v)
        };
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = {
            let mut v = FxHashSet::default();
            decompose_sp(&fg.graph, addr, sp, &mut memo, &mut v)
        };
        assert!(matches!((&r1, &r2),
            (Some(SpExpr::Terminal { offset: -4, .. }),
             Some(SpExpr::Terminal { offset: -4, .. }))));
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
        // An IntConst is not SP-rooted.
        let sp = sp();
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let c = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_return(Some(c), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        assert!(decompose_sp(&fg.graph, c, sp, &mut memo, &mut visiting).is_none());
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_caches_intermediate_results() -> crate::Result<()> {
        // Edge case: decomposing the outermost node of a deep `sp - K1 - K2 - K3`
        // chain must populate the memo for ALL intermediate sub-expressions, so
        // a sibling walk hitting any of them gets a cache hit. The previous
        // `if visiting.is_empty()` predicate only fired at the outermost call
        // frame, so intermediates were never cached and the memo was useless
        // for cross-call sharing.
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        let twelve = b.build_int_const(12u64, NodeOutputType::U32)?;
        let s1 = b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
        let s2 = b.build_int_sub(s1, eight, NodeOutputType::U32)?;
        let s3 =
            b.build_int_sub(s2, twelve, NodeOutputType::U32)?;
        b.build_return(Some(s3), &[])?;
        let fg = b.build()?;

        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, s3, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -24, .. })));

        // After one top-level walk, all three intermediate outputs must be
        // memoized. (sp_val itself is cached too, but its NodeOutputId is
        // VarPhi-of-InitialVar, which we don't directly check here.)
        assert!(memo.contains_key(&s3), "expected memo entry for s3");
        assert!(memo.contains_key(&s2), "expected memo entry for s2");
        assert!(memo.contains_key(&s1), "expected memo entry for s1");
        Ok(())
    }

    #[test]
    fn decompose_sp_does_not_cache_none_results() -> crate::Result<()> {
        // Edge case: a `None` verdict could be either "genuinely not SP-rooted"
        // (safe to recompute) or "cycle-truncated on this call path" (must not
        // be cached, because a different call path may resolve it). Caching
        // None conservatively for both cases would be wrong for the cycle case.
        // The simpler invariant — never cache None — is what we assert here.
        let sp = sp();
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let c = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_return(Some(c), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, c, sp, &mut memo, &mut visiting);
        assert!(r.is_none());
        assert!(
            !memo.contains_key(&c),
            "decompose_sp must not cache None verdicts (cycle-truncation cannot be distinguished from genuine 'not SP-rooted' here)"
        );
        Ok(())
    }

    #[test]
    fn decompose_sp_phi_with_non_sp_pred_returns_none() -> crate::Result<()> {
        // A VarPhi(sp) whose predecessor value is NOT SP-rooted must
        // decompose to None.  Previously decompose_sp_phi fabricated a
        // Terminal{base: phi_output, offset: 0} on this path; callers
        // ignored `base` but trusted `offset == 0`, which on conventions
        // where stack_arg_offsets[0] == 0 (AArch64/ARM AAPCS) could
        // misclassify a non-SP-rooted phi as the first stack argument or
        // wrongly forward a load over it.
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        // entry: if cond goto a else goto bb
        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: sp = sp - 4 (SP-rooted)
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_minus_4 =
            b.build_int_sub(sp_a, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: sp = 0xDEAD_BEEF (NOT SP-rooted — a literal value pretending
        // to be a new SP).
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

        // c: read sp.  The phi at c has two predecessor values: the SP-rooted
        // one from `a` and the bogus const from `bb`.  decompose_sp must
        // refuse to claim "this is sp + K" for that phi.
        b.set_region(c);
        let sp_at_c = b.read_variable(&sp)?;
        b.build_return(Some(sp_at_c), &[])?;
        let fg = b.build()?;

        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, sp_at_c, sp, &mut memo, &mut visiting);
        assert!(
            r.is_none(),
            "expected None for VarPhi(sp) with a non-SP-rooted predecessor, got {r:?}"
        );
        Ok(())
    }

    /// FreeBSD i386 10.0 prologue: `and $0xfffffff8, %esp` aligns the
    /// stack to 8 bytes after the saved-register pushes.  All subsequent
    /// stack arithmetic is anchored at the And's output, not at
    /// `InitialVar(sp)`, so `decompose_sp` must recognise the And and
    /// treat its output as a stable opaque base (offset 0) — otherwise
    /// every store after the alignment dance is a non-decomposable
    /// `Store(_)`, and `CallStackArgCollect` walks past the call's args
    /// as "non-aliasing".
    #[test]
    fn decompose_sp_and_with_alignment_mask_yields_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        // Simulate `and $0xfffffff8, %esp`.
        let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::U32)?;
        let aligned = b.build_int_binary_operation(
            sp_val, mask, IntBinaryOp::And, NodeOutputType::U32)?;
        b.build_return(Some(aligned), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg.graph, aligned, sp, &mut memo, &mut visiting);
        // The aligned output is a stable opaque base.  Offset = 0
        // because the alignment can shift the value by 0..7 bytes — we
        // can't pin a constant delta, but we *can* pin a stable
        // `NodeOutputId` that subsequent decompositions reference.
        let Some(SpExpr::Terminal { base, offset }) = r else {
            panic!("expected Terminal from And-aligned SP, got {r:?}");
        };
        assert_eq!(offset, 0, "And-aligned base offset must be 0");
        // Base must NOT be the InitialVar(sp) output — it's the And output.
        let base_node = fg.graph.get_node_from_output(base);
        assert!(
            matches!(*fg.graph.node_kind(base_node), NodeKind::IntBinaryOp(IntBinaryOp::And)),
            "And-aligned base must point to the And node, got {:?}",
            fg.graph.node_kind(base_node)
        );
        Ok(())
    }

    /// Following the alignment dance, the function does
    /// `sub $0x1d0, %esp` (the local-frame reservation).  The post-Sub
    /// SP must decompose to the *same* opaque base (the And output),
    /// just with a non-zero offset.  Without this, every cdecl call
    /// site after the alignment dance has args at addresses that
    /// `decompose_sp` cannot relate to each other, breaking
    /// `CallStackArgCollect`.
    #[test]
    fn decompose_sp_sub_after_and_chains_offset_through_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[sp], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::U32)?;
        let aligned = b.build_int_binary_operation(
            sp_val, mask, IntBinaryOp::And, NodeOutputType::U32)?;
        let frame = b.build_int_const(0x1D0u64, NodeOutputType::U32)?;
        let post_sub = b.build_int_sub(aligned, frame, NodeOutputType::U32)?;
        b.build_return(Some(post_sub), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let aligned_dec = decompose_sp(&fg.graph, aligned, sp, &mut memo, &mut visiting)
            .expect("aligned must decompose");
        let post_sub_dec = decompose_sp(&fg.graph, post_sub, sp, &mut memo, &mut visiting)
            .expect("post_sub must decompose");
        let SpExpr::Terminal { base: aligned_base, offset: aligned_off } = aligned_dec else {
            panic!("aligned must be Terminal");
        };
        let SpExpr::Terminal { base: post_sub_base, offset: post_sub_off } = post_sub_dec else {
            panic!("post_sub must be Terminal");
        };
        assert_eq!(
            aligned_base, post_sub_base,
            "post-Sub base must equal post-And base (opaque base shared)"
        );
        assert_eq!(aligned_off, 0);
        assert_eq!(post_sub_off, -0x1D0, "Sub by 0x1D0 shifts offset by -0x1D0");
        Ok(())
    }
}
