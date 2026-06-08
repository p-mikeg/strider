//! Stack-pointer expression decomposer.
//!
//! `decompose_sp` is the workhorse: given an output that is `InitialVar(sp)`
//! transformed by `Add` of constants (subtraction appears as `Add(_, Neg(K))`)
//! or anchored at an alignment-masked `sp & mask`, it returns a single
//! `SpExpr { base, offset }` terminal (or `None`).
//! Callers thread a per-pass-call memo through it so repeated walks over the
//! same SP chain cost O(1) on cache hit.
//!
//! The decomposer does **not** look through `Phi` nodes — a stack-tagged
//! `Phi(sp)` (loop-header join, or the single-predecessor phi the lifter wraps
//! around `read_variable(sp)`) decomposes to `None`.  By the time any SP-aware
//! pass runs `decompose_sp`, `PhiCollapse` / `RedundantPhis` have already
//! collapsed those single-predecessor phis to their `InitialVar(sp)` input, so
//! the decomposer only ever meets real terminals.  A `None` reads as "not a
//! provable SP terminal", which every caller already treats conservatively
//! (may-alias / opaque base).

use rustc_hash::FxHashMap;

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IntBinaryOp};
use strider_ir::{IRViewer, IRWalker};

/// Decomposed stack-pointer expression: `base + offset`, where `base` is an
/// SP-rooted node (`InitialVar(sp)` or an alignment-masked SP `And` output).
///
/// `decompose_sp` returns `Option<SpExpr>`; `None` carries the
/// "not a provable SP terminal" case, so there is no separate variant for it.
#[derive(Clone, Copy, Debug)]
pub struct SpExpr {
    pub base: ValueId,
    pub offset: i64,
}

impl SpExpr {
    #[must_use]
    pub(crate) fn shifted(self, delta: i64) -> Self {
        SpExpr {
            base: self.base,
            offset: self.offset.wrapping_add(delta),
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
pub(crate) fn int_const_signed(function: &Function, value: ValueId) -> Option<i64> {
    if let Some(c) = function.int_const_val(value) {
        // `value` is an `IntConst`, so its output is always a value type;
        // `get_signed_int` can still fail for wide (>128-bit) types.
        let ty = function
            .value_kind(value)
            .as_value()
            .expect("IntConst output is a value");
        let signed = ty.get_signed_int(u128::from(c))?;
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
    let node = function.producer(value);
    if matches!(
        function.node_kind(node),
        NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::Neg)
    ) {
        // IntUnaryOp has exactly 1 input (validated structural invariant).
        let inner = function
            .node_inputs_exact::<1>(node)
            .expect("IntUnaryOp(Neg) has 1 input (validated)")[0];
        let k = function.int_const_val(inner)?;
        // `inner` is an `IntConst` (checked above), so its output is a value.
        let inner_ty = function
            .value_kind(inner)
            .as_value()
            .expect("IntConst output is a value");
        let neg_raw = u128::from(k).wrapping_neg();
        let neg_masked = inner_ty.get_unsigned_int(neg_raw)?;
        let signed = inner_ty.get_signed_int(neg_masked)?;
        return i64::try_from(signed).ok();
    }
    None
}

/// Per-pass-call memo for `decompose_sp`.
pub type SpExprMemo = FxHashMap<ValueId, Option<SpExpr>>;

/// Stack-pointer expression decomposer: holds the `function`, the `stack_vn`
/// to anchor on, and a per-pass-call `memo`.  Production callers construct it
/// via [`SpDecomposer::new`] (which derives `stack_vn` from the function's
/// calling convention); tests that decompose against a non-default stack
/// varnode use [`SpDecomposer::with_stack_vn`].
pub(crate) struct SpDecomposer<'a> {
    function: &'a Function,
    stack_vn: rsleigh::Vn,
    memo: &'a mut SpExprMemo,
}

impl<'a> SpDecomposer<'a> {
    /// Derives `stack_vn` from the function's calling convention — the
    /// production path (every pass decomposes against `default_cc().stack_vn`).
    pub(crate) fn new(function: &'a Function, memo: &'a mut SpExprMemo) -> Self {
        let stack_vn = function.default_cc().stack_vn;
        Self {
            function,
            stack_vn,
            memo,
        }
    }

    /// Explicit `stack_vn` — for tests (and any caller) that decompose
    /// against a stack varnode not equal to `default_cc().stack_vn`.
    #[cfg(test)]
    pub(crate) fn with_stack_vn(
        function: &'a Function,
        stack_vn: rsleigh::Vn,
        memo: &'a mut SpExprMemo,
    ) -> Self {
        Self {
            function,
            stack_vn,
            memo,
        }
    }

    /// Decomposes `value` into `InitialVar(sp) + K` (or per-branch equivalent),
    /// caching definitive results in the memo.
    ///
    /// Implemented as a single defs-before-uses (reverse-post-order) sweep over
    /// the address cone: because every operand is classified before the node
    /// that consumes it, each arm is a local map lookup.  `Phi` nodes are not
    /// SP terminals (they classify to `None`), so the cone the sweep traverses
    /// is a DAG of `InitialVar` / `Add` / `And` nodes.
    pub(crate) fn decompose(&mut self, value: ValueId) -> Option<SpExpr> {
        if let Some(cached) = self.memo.get(&value) {
            return *cached;
        }
        let graph = self.function.graph();
        let rpo = match self.function.walk_info(Some(graph.producer(value))) {
            Some(info) => self.function.reverse_postorder(&info),
            None => Vec::new(),
        };
        for node in rpo {
            let Ok([node_out]) = self.function.node_outputs_exact::<1>(node) else {
                continue;
            };
            if self.memo.contains_key(&node_out) {
                continue;
            }
            let expr = self.classify_sp_node(node, node_out);
            // Mirror the legacy contract: never cache a `None` verdict (a
            // cycle-truncated branch may resolve on a different call path).
            if expr.is_some() {
                self.memo.insert(node_out, expr);
            }
        }
        self.memo.get(&value).copied().flatten()
    }

    /// Classifies a single node in the address cone given that all of its
    /// operands have already been classified into the memo (guaranteed by the
    /// defs-before-uses `rpo` order).  `Phi` is not an SP terminal and falls
    /// through to `None`.
    fn classify_sp_node(&self, node: NodeId, node_value: ValueId) -> Option<SpExpr> {
        let function = self.function;
        match *function.node_kind(node) {
            NodeKind::InitialVar(vn) if vn == self.stack_vn => Some(SpExpr {
                base: node_value,
                offset: 0,
            }),
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                // IntBinaryOp has exactly 2 inputs (validated structural invariant).
                let [l, r] = function
                    .node_inputs_exact::<2>(node)
                    .expect("IntBinaryOp(Add) has 2 inputs (validated)");
                if let Some(c) = int_const_signed(function, r) {
                    return self.memo.get(&l).copied().flatten().map(|e| e.shifted(c));
                }
                if let Some(c) = int_const_signed(function, l) {
                    return self.memo.get(&r).copied().flatten().map(|e| e.shifted(c));
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
                // IntBinaryOp has exactly 2 inputs (validated structural invariant).
                let [l, r] = function
                    .node_inputs_exact::<2>(node)
                    .expect("IntBinaryOp(And) has 2 inputs (validated)");
                let sp_value = if int_const_signed(function, r).is_some() {
                    l
                } else if int_const_signed(function, l).is_some() {
                    r
                } else {
                    return None;
                };
                // The And's output is a fresh opaque base (offset 0) for
                // downstream walkers; we only require the non-mask operand to
                // be SP-rooted, discarding its concrete decomposition.
                self.memo.get(&sp_value).copied().flatten().map(|_| SpExpr {
                    base: node_value,
                    offset: 0,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::IRBuilderExt;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    /// Collapses the single-predecessor `read_variable(sp)` phi so an SP
    /// address becomes a bare `InitialVar(sp) + k` terminal — the shape
    /// `decompose_sp` sees in production (it no longer looks through phis;
    /// the pipeline's `PhiCollapse` has run by then).
    fn collapse_phis(fg: &mut strider_ir::Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(fg, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    #[test]
    fn int_const_signed_u32_negative() -> crate::Result<()> {
        // 0xFFFF_FFFC at I32 must read as -4 signed.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
        b.build_return(Some(v), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert_eq!(int_const_signed(&fg, v), Some(-4));
        Ok(())
    }

    #[test]
    fn int_const_signed_neg_of_min_uses_modular_negation() -> crate::Result<()> {
        // `Neg(IntConst(0x8000_0000_U32))` must agree with the IR's modular
        // semantics: in two's-complement at I32, `-(-2^31) = -2^31`.  The
        // peephole here must NOT return `+2^31` (which is what
        // `checked_neg(-2^31i128)` produces) — that would silently disagree
        // with `ConstantFold::IntUnaryOp::Neg`'s `wrapping_neg` evaluator
        // and `StackOffsetDetect` could classify the same store inconsistently
        // depending on whether the inner Neg had been folded yet.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let inner = b.build_int_const(0x8000_0000u64, ValueType::I32)?;
        let neg =
            b.build_int_unary_operation(inner, strider_ir::IntUnaryOp::Neg, ValueType::I32)?;
        b.build_return(Some(neg), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        // Modular: wrapping_neg(0x8000_0000) = 0x8000_0000 → sign-extended to i32 = -2^31.
        assert_eq!(int_const_signed(&fg, neg), Some(i32::MIN.into()));
        Ok(())
    }

    #[test]
    fn int_const_signed_neg_of_positive_const() -> crate::Result<()> {
        // Sanity: `Neg(IntConst(7_U32))` peeps through to `-7`.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let inner = b.build_int_const(7u64, ValueType::I32)?;
        let neg =
            b.build_int_unary_operation(inner, strider_ir::IntUnaryOp::Neg, ValueType::I32)?;
        b.build_return(Some(neg), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert_eq!(int_const_signed(&fg, neg), Some(-7));
        Ok(())
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        b.build_return(Some(sp_val), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        // `read_variable(sp)` wraps `InitialVar(sp)` in a single-predecessor
        // phi; PhiCollapse collapses it, so the live SP value (the Return's
        // value input) is the bare `InitialVar(sp)` that decomposes to
        // offset 0.  (Decomposing the now-detached phi output would be None.)
        collapse_phis(&mut fg);
        let ret = fg
            .graph()
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
            .expect("return");
        let live_sp = fg.node_inputs(ret)[2];
        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(live_sp);
        assert!(matches!(r, Some(SpExpr { offset: 0, .. })));
        let _ = sp_val;
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, four, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::Result<()> {
        // Add(sp, 0xFFFF_FFFC_U32) must decompose to -4 (sign-extended).
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let neg_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
        let addr =
            b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_hit_returns_same_result() -> crate::Result<()> {
        // Calling decompose_sp twice on the same out should populate the memo
        // and return the same answer.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_val, four, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let r1 = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(addr);
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(addr);
        assert!(matches!(
            (&r1, &r2),
            (
                Some(SpExpr { offset: -4, .. }),
                Some(SpExpr { offset: -4, .. })
            )
        ));
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
        // An IntConst is not SP-rooted.
        let sp = sp();
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        assert!(SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(c).is_none());
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
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let eight = b.build_int_const(8u64, ValueType::I32)?;
        let twelve = b.build_int_const(12u64, ValueType::I32)?;
        let s1 = b.build_sub_as_add_neg(sp_val, four, ValueType::I32)?;
        let s2 = b.build_sub_as_add_neg(s1, eight, ValueType::I32)?;
        let s3 = b.build_sub_as_add_neg(s2, twelve, ValueType::I32)?;
        b.build_return(Some(s3), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(s3);
        assert!(matches!(r, Some(SpExpr { offset: -24, .. })));

        // After one top-level walk, all three intermediate outputs must be
        // memoized. (sp_val itself is cached too, but its ValueId is
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
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(c);
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
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let sp_minus_4 = b.build_sub_as_add_neg(sp_a, four, ValueType::I32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: sp = 0xDEAD_BEEF (NOT SP-rooted — a literal value pretending
        // to be a new SP).
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

        // c: read sp.  The phi at c has two predecessor values: the SP-rooted
        // one from `a` and the bogus const from `bb`.  decompose_sp must
        // refuse to claim "this is sp + K" for that phi.
        b.set_region(c);
        let sp_at_c = b.read_variable(&sp)?;
        b.build_return(Some(sp_at_c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(sp_at_c);
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
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // Simulate `and $0xfffffff8, %esp`.
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        b.build_return(Some(aligned), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(aligned);
        // The aligned output is a stable opaque base.  Offset = 0
        // because the alignment can shift the value by 0..7 bytes — we
        // can't pin a constant delta, but we *can* pin a stable
        // `ValueId` that subsequent decompositions reference.
        let Some(SpExpr { base, offset }) = r else {
            panic!("expected Terminal from And-aligned SP, got {r:?}");
        };
        assert_eq!(offset, 0, "And-aligned base offset must be 0");
        // Base must NOT be the InitialVar(sp) output — it's the And output.
        let base_node = fg.producer(base);
        assert!(
            matches!(
                *fg.node_kind(base_node),
                NodeKind::IntBinaryOp(IntBinaryOp::And)
            ),
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
    fn decompose_sp_sub_after_and_chains_offset_through_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        let frame = b.build_int_const(0x1D0u64, ValueType::I32)?;
        let post_sub = b.build_sub_as_add_neg(aligned, frame, ValueType::I32)?;
        b.build_return(Some(post_sub), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let aligned_dec =
            SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(aligned).expect("aligned must decompose");
        let post_sub_dec =
            SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(post_sub).expect("post_sub must decompose");
        let SpExpr {
            base: aligned_base,
            offset: aligned_off,
        } = aligned_dec;
        let SpExpr {
            base: post_sub_base,
            offset: post_sub_off,
        } = post_sub_dec;
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
    fn decompose_sp_deep_and_chain_terminates_without_overflow() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        const N: usize = 6000;
        for _ in 0..N {
            current =
                b.build_int_binary_operation(current, mask, IntBinaryOp::And, ValueType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        // Iterative rpo sweep: the deep And chain re-bases at each level and
        // resolves to an opaque base without recursion, so no stack overflow.
        let r = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(current);
        assert!(matches!(r, Some(SpExpr { offset: 0, .. })));
        Ok(())
    }

    /// Regression: `decompose_sp`
    /// must not blow the thread stack on a deep `sp + K1 + K2 + ... + KN`
    /// chain.  The recursive form overflowed at ~4-8k nodes; the
    /// iterative form must walk a 5000-node chain without panic AND
    /// produce the correct cumulative offset.
    #[test]
    fn decompose_sp_does_not_stack_overflow_on_deep_chain() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        const N: usize = 5000;
        for _ in 0..N {
            let one = b.build_int_const(1u64, ValueType::I32)?;
            current =
                b.build_int_binary_operation(current, one, IntBinaryOp::Add, ValueType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let SpExpr { offset, .. } = SpDecomposer::with_stack_vn(&fg, sp, &mut memo).decompose(current)
            .expect("5000-node chain must decompose without stack-overflowing");
        assert_eq!(
            offset, N as i64,
            "cumulative offset must equal N adds of +1"
        );
        Ok(())
    }
}
