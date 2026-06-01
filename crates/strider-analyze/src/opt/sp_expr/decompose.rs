//! Stack-pointer expression decomposer.
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

use rustc_hash::FxHashMap;

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};
use strider_ir::{Function, Graph, IntBinaryOp};

/// Decomposed stack-pointer expression.
///
/// `pub` so out-of-crate callers (e.g. the indirect-branch classifier
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
/// `StackOffsetDetect`'s ability to make progress on the same iteration.
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
    // and `StackOffsetDetect` could classify the same store inconsistently.
    let node = g.node_for_output(out);
    if matches!(g.node_kind(node), NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::Neg)) {
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
/// caching definitive results in `memo`.
///
/// Implemented as a single defs-before-uses (`Graph::rpo`) sweep over the
/// address cone: because every operand is classified before the node that
/// consumes it, each arm is a local map lookup. Cyclic `Phi(sp)` back-edges
/// are the only non-DAG edge; a back-edge whose source is not yet classified
/// when the phi is processed is treated as "unknown," which collapses the
/// phi to `None` unless every predecessor independently resolves to the same
/// `Terminal` (matching the prior recursive contract).
pub fn decompose_sp(
    function: &Function,
    out: NodeOutputId,
    stack_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> Option<SpExpr> {
    if let Some(cached) = memo.get(&out) {
        return cached.clone();
    }
    for node in function.graph().rpo(out) {
        let Ok([node_out]) = function.node_outputs_exact::<1>(node) else {
            continue;
        };
        if memo.contains_key(&node_out) {
            continue;
        }
        let expr = classify_sp_node(function, node, node_out, stack_vn, memo);
        // Mirror the legacy contract: never cache a `None` verdict (a
        // cycle-truncated branch may resolve on a different call path).
        if expr.is_some() {
            memo.insert(node_out, expr);
        }
    }
    memo.get(&out).cloned().flatten()
}

/// Classifies a single node in the address cone given that all of its
/// operands have already been classified into `memo` (guaranteed by the
/// defs-before-uses `rpo` order, except for `Phi` back-edges which read
/// whatever the map currently holds).
fn classify_sp_node(
    function: &Function,
    node: NodeId,
    node_out: NodeOutputId,
    stack_vn: rsleigh::Vn,
    memo: &SpExprMemo,
) -> Option<SpExpr> {
    match *function.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == stack_vn => Some(SpExpr::Terminal {
            base: node_out,
            offset: 0,
        }),
        NodeKind::Phi if function.phi_var_tag(node) == Some(stack_vn) => {
            classify_sp_phi(function, node, memo)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            let inputs = function.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let (l, r) = (inputs[0], inputs[1]);
            if let Some(c) = int_const_signed(function, r) {
                return memo.get(&l).cloned().flatten().map(|e| e.shifted(c));
            }
            if let Some(c) = int_const_signed(function, l) {
                return memo.get(&r).cloned().flatten().map(|e| e.shifted(c));
            }
            None
        }
        // x86 cdecl alignment dance: `and $0xfffffff8, %esp` (or wider
        // `0xfffffff0` for SSE-aligned frames).  The And's output is
        // runtime-aligned `(SP & mask)` — its exact value depends on the
        // entry SP's alignment, so the offset relative to `InitialVar(sp)`
        // is unknown.  But within the function the And's output is *fixed*
        // and serves as a stable opaque base for every subsequent stack
        // address.  Return `Terminal { base: <And output>, offset: 0 }`
        // so downstream Adds / Subs of constants chain through normally
        // and `StackOffsetDetect` can classify the post-alignment stores
        // as stack-aliased using this base.
        //
        // Only matches when the non-mask operand is itself an SP-rooted
        // expression — guards against `And(rax, mask)` accidentally
        // producing a fake stack base.
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let inputs = function.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let (l, r) = (inputs[0], inputs[1]);
            let sp_input = if int_const_signed(function, r).is_some() {
                l
            } else if int_const_signed(function, l).is_some() {
                r
            } else {
                return None;
            };
            // The And's output is a fresh opaque base (offset 0) for
            // downstream walkers; we only require the non-mask operand to
            // be SP-rooted, discarding its concrete decomposition.
            memo.get(&sp_input)
                .cloned()
                .flatten()
                .map(|_| SpExpr::Terminal {
                    base: node_out,
                    offset: 0,
                })
        }
        _ => None,
    }
}

/// Classifies a `Phi(sp)` from its already-classified predecessor values.
/// Every predecessor must resolve to a `Terminal`; a predecessor still
/// unclassified (loop back-edge) or non-`Terminal` collapses the phi to
/// `None`.
fn classify_sp_phi(function: &Function, node: NodeId, memo: &SpExprMemo) -> Option<SpExpr> {
    let inputs = function.node_inputs(node);
    if inputs.len() < 2 {
        return None;
    }
    let mut bases = Vec::with_capacity(inputs.len() - 1);
    let mut offsets = Vec::with_capacity(inputs.len() - 1);
    for pred in inputs.into_iter().skip(1) {
        let Some(SpExpr::Terminal { base, offset }) = memo.get(&pred).cloned().flatten() else {
            return None;
        };
        bases.push(base);
        offsets.push(offset);
    }
    if bases.iter().all(|&b| b == bases[0]) && offsets.iter().all(|&o| o == offsets[0]) {
        Some(SpExpr::Terminal {
            base: bases[0],
            offset: offsets[0],
        })
    } else {
        Some(SpExpr::Phi {
            phi_node: node,
            offsets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::NodeOutputType;
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};
    use strider_ir::{FunctionBuilder, IntBinaryOp};

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    #[test]
    fn int_const_signed_u32_negative() -> crate::opt::Result<()> {
        // 0xFFFF_FFFC at I32 must read as -4 signed.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(0xFFFF_FFFCu64, NodeOutputType::I32)?;
        b.build_return(Some(v), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        assert_eq!(int_const_signed(fg.graph(), v), Some(-4));
        Ok(())
    }

    #[test]
    fn int_const_signed_neg_of_min_uses_modular_negation() -> crate::opt::Result<()> {
        // `Neg(IntConst(0x8000_0000_U32))` must agree with the IR's modular
        // semantics: in two's-complement at I32, `-(-2^31) = -2^31`.  The
        // peephole here must NOT return `+2^31` (which is what
        // `checked_neg(-2^31i128)` produces) — that would silently disagree
        // with `ConstantFold::IntUnaryOp::Neg`'s `wrapping_neg` evaluator
        // and `StackOffsetDetect` could classify the same store inconsistently
        // depending on whether the inner Neg had been folded yet.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let inner = b.build_int_const(0x8000_0000u64, NodeOutputType::I32)?;
        let neg = b.build_int_unary_operation(inner, strider_ir::IntUnaryOp::Neg, NodeOutputType::I32)?;
        b.build_return(Some(neg), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        // Modular: wrapping_neg(0x8000_0000) = 0x8000_0000 → sign-extended to i32 = -2^31.
        assert_eq!(int_const_signed(fg.graph(), neg), Some(i32::MIN.into()));
        Ok(())
    }

    #[test]
    fn int_const_signed_neg_of_positive_const() -> crate::opt::Result<()> {
        // Sanity: `Neg(IntConst(7_U32))` peeps through to `-7`.
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let inner = b.build_int_const(7u64, NodeOutputType::I32)?;
        let neg = b.build_int_unary_operation(inner, strider_ir::IntUnaryOp::Neg, NodeOutputType::I32)?;
        b.build_return(Some(neg), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        assert_eq!(int_const_signed(fg.graph(), neg), Some(-7));
        Ok(())
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::opt::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        b.build_return(Some(sp_val), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        // sp_val is a VarPhi-of-InitialVar; the phi has 1 predecessor →
        // collapses to Terminal{base: InitialVar(sp), offset: 0}.
        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, sp_val, sp, &mut memo);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: 0, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::opt::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, four, NodeOutputType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, addr, sp, &mut memo);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::opt::Result<()> {
        // Add(sp, 0xFFFF_FFFC_U32) must decompose to -4 (sign-extended).
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let neg_four = b.build_int_const(0xFFFF_FFFCu64, NodeOutputType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, NodeOutputType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, addr, sp, &mut memo);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_hit_returns_same_result() -> crate::opt::Result<()> {
        // Calling decompose_sp twice on the same out should populate the memo
        // and return the same answer.
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, four, NodeOutputType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r1 = decompose_sp(&fg, addr, sp, &mut memo);
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = decompose_sp(&fg, addr, sp, &mut memo);
        assert!(matches!((&r1, &r2),
            (Some(SpExpr::Terminal { offset: -4, .. }),
             Some(SpExpr::Terminal { offset: -4, .. }))));
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::opt::Result<()> {
        // An IntConst is not SP-rooted.
        let sp = sp();
        let mut b = FunctionBuilder::empty()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, NodeOutputType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        assert!(decompose_sp(&fg, c, sp, &mut memo).is_none());
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_caches_intermediate_results() -> crate::opt::Result<()> {
        // Edge case: decomposing the outermost node of a deep `sp - K1 - K2 - K3`
        // chain must populate the memo for ALL intermediate sub-expressions, so
        // a sibling walk hitting any of them gets a cache hit. The previous
        // `if visiting.is_empty()` predicate only fired at the outermost call
        // frame, so intermediates were never cached and the memo was useless
        // for cross-call sharing.
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::I32)?;
        let eight = b.build_int_const(8u64, NodeOutputType::I32)?;
        let twelve = b.build_int_const(12u64, NodeOutputType::I32)?;
        let s1 = b.build_sub_as_add_neg(sp_val, four, NodeOutputType::I32)?;
        let s2 = b.build_sub_as_add_neg(s1, eight, NodeOutputType::I32)?;
        let s3 =
            b.build_sub_as_add_neg(s2, twelve, NodeOutputType::I32)?;
        b.build_return(Some(s3), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;

        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, s3, sp, &mut memo);
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
    fn decompose_sp_does_not_cache_none_results() -> crate::opt::Result<()> {
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
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, NodeOutputType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, c, sp, &mut memo);
        assert!(r.is_none());
        assert!(
            !memo.contains_key(&c),
            "decompose_sp must not cache None verdicts (cycle-truncation cannot be distinguished from genuine 'not SP-rooted' here)"
        );
        Ok(())
    }

    #[test]
    fn decompose_sp_phi_with_non_sp_pred_returns_none() -> crate::opt::Result<()> {
        // A VarPhi(sp) whose predecessor value is NOT SP-rooted must
        // decompose to None.  Previously decompose_sp_phi fabricated a
        // Terminal{base: phi_output, offset: 0} on this path; callers
        // ignored `base` but trusted `offset == 0`, which on conventions
        // where stack_arg_offsets[0] == 0 (AArch64/ARM AAPCS) could
        // misclassify a non-SP-rooted phi as the first stack argument or
        // wrongly forward a load over it.
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn()?;
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
        let four = b.build_int_const(4u64, NodeOutputType::I32)?;
        let sp_minus_4 =
            b.build_sub_as_add_neg(sp_a, four, NodeOutputType::I32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: sp = 0xDEAD_BEEF (NOT SP-rooted — a literal value pretending
        // to be a new SP).
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::I32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

        // c: read sp.  The phi at c has two predecessor values: the SP-rooted
        // one from `a` and the bogus const from `bb`.  decompose_sp must
        // refuse to claim "this is sp + K" for that phi.
        b.set_region(c);
        let sp_at_c = b.read_variable(&sp)?;
        b.build_return(Some(sp_at_c), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;

        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, sp_at_c, sp, &mut memo);
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
    fn decompose_sp_and_with_alignment_mask_yields_opaque_base() -> crate::opt::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // Simulate `and $0xfffffff8, %esp`.
        let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::I32)?;
        let aligned = b.build_int_binary_operation(
            sp_val, mask, IntBinaryOp::And, NodeOutputType::I32)?;
        b.build_return(Some(aligned), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, aligned, sp, &mut memo);
        // The aligned output is a stable opaque base.  Offset = 0
        // because the alignment can shift the value by 0..7 bytes — we
        // can't pin a constant delta, but we *can* pin a stable
        // `NodeOutputId` that subsequent decompositions reference.
        let Some(SpExpr::Terminal { base, offset }) = r else {
            panic!("expected Terminal from And-aligned SP, got {r:?}");
        };
        assert_eq!(offset, 0, "And-aligned base offset must be 0");
        // Base must NOT be the InitialVar(sp) output — it's the And output.
        let base_node = fg.node_for_output(base);
        assert!(
            matches!(*fg.node_kind(base_node), NodeKind::IntBinaryOp(IntBinaryOp::And)),
            "And-aligned base must point to the And node, got {:?}",
            fg.node_kind(base_node)
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
    fn decompose_sp_sub_after_and_chains_offset_through_opaque_base() -> crate::opt::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::I32)?;
        let aligned = b.build_int_binary_operation(
            sp_val, mask, IntBinaryOp::And, NodeOutputType::I32)?;
        let frame = b.build_int_const(0x1D0u64, NodeOutputType::I32)?;
        let post_sub = b.build_sub_as_add_neg(aligned, frame, NodeOutputType::I32)?;
        b.build_return(Some(post_sub), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let aligned_dec = decompose_sp(&fg, aligned, sp, &mut memo)
            .expect("aligned must decompose");
        let post_sub_dec = decompose_sp(&fg, post_sub, sp, &mut memo)
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

    /// Deep nested-`And` shape: the iterative `rpo` sweep re-bases at
    /// each level and resolves to an opaque base without recursion, so a
    /// pathologically deep chain terminates cleanly (no stack overflow,
    /// no recursion-depth budget) with an opaque `Terminal` base.
    #[test]
    fn decompose_sp_deep_and_chain_terminates_without_overflow() -> crate::opt::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, NodeOutputType::I32)?;
        const N: usize = 6000;
        for _ in 0..N {
            current = b.build_int_binary_operation(
                current, mask, IntBinaryOp::And, NodeOutputType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        // Iterative rpo sweep: the deep And chain re-bases at each level and
        // resolves to an opaque base without recursion, so no stack overflow.
        let r = decompose_sp(&fg, current, sp, &mut memo);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: 0, .. })));
        Ok(())
    }

    /// Regression: `decompose_sp`
    /// must not blow the thread stack on a deep `sp + K1 + K2 + ... + KN`
    /// chain.  The recursive form overflowed at ~4-8k nodes; the
    /// iterative form must walk a 5000-node chain without panic AND
    /// produce the correct cumulative offset.
    #[test]
    fn decompose_sp_does_not_stack_overflow_on_deep_chain() -> crate::opt::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new().tracked(sp).arg(sp).build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        const N: usize = 5000;
        for _ in 0..N {
            let one = b.build_int_const(1u64, NodeOutputType::I32)?;
            current = b.build_int_binary_operation(current, one, IntBinaryOp::Add, NodeOutputType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r = decompose_sp(&fg, current, sp, &mut memo)
            .expect("5000-node chain must decompose without stack-overflowing");
        let SpExpr::Terminal { offset, .. } = r else {
            panic!("expected Terminal, got {r:?}");
        };
        assert_eq!(offset, N as i64, "cumulative offset must equal N adds of +1");
        Ok(())
    }
}
