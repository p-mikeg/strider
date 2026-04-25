//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`stack_store::detect`, `stack_load_forward`, `function_args::stack_args`).
//!
//! `decompose_sp` is the workhorse: given an output that may be `InitialVar(sp)`
//! transformed by `Add`/`Sub` of constants and joined by `ControlPhi(sp)`, it
//! returns either a `Terminal { base, offset }` or a `Phi { node, offsets[] }`.
//! Callers thread a per-pass-call memo through it so repeated walks over the
//! same SP chain cost O(1) on cache hit.

use rustc_hash::{FxHashMap, FxHashSet};

use ir::node::{NodeId, NodeKind, NodeOutputId};
use ir::{BuiltFunctionGraph, IntBinaryOp};

/// Decomposed stack-pointer expression.
#[derive(Clone, Debug)]
pub(crate) enum SpExpr {
    /// `base + offset`, where `base` is an SP-rooted node.
    Terminal { base: NodeOutputId, offset: i64 },
    /// `ControlPhi(stack_ptr)` where every predecessor resolves to
    /// `InitialVar(stack_ptr) + offsets[j]`.
    Phi { phi_node: NodeId, offsets: Vec<i64> },
}

impl SpExpr {
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
#[inline]
pub(crate) fn ranges_disjoint(a_off: i64, a_size: i64, b_off: i64, b_size: i64) -> bool {
    a_off + a_size <= b_off || b_off + b_size <= a_off
}

/// Reads an integer-constant output as signed, sign-extended from its declared
/// bit width. Returns `None` for non-integer-constant or for U128/U256.
pub(crate) fn int_const_signed(fg: &BuiltFunctionGraph, out: NodeOutputId) -> Option<i64> {
    let c = fg.int_const_val(out)?;
    fg.graph.output_kind(out).as_value()?.get_signed_int(c)
}

/// Per-pass-call memo for `decompose_sp`.
pub(crate) type SpExprMemo = FxHashMap<NodeOutputId, Option<SpExpr>>;

/// Decomposes `out` into `InitialVar(sp) + K` (or per-branch equivalent),
/// caching definitive results in `memo`. The `visiting` set guards against
/// cycles through `ControlPhi` back-edges; cycle-broken results are NOT
/// memoized (so a different call path can still resolve the same output).
pub(crate) fn decompose_sp(
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut FxHashSet<NodeId>,
) -> Option<SpExpr> {
    if let Some(cached) = memo.get(&out) {
        return cached.clone();
    }
    let node = fg.graph.get_node_from_output(out);
    if !visiting.insert(node) {
        // Cycle: do NOT cache (a different call path may resolve it).
        return None;
    }
    let result = decompose_sp_inner(fg, out, node, sp_vn, memo, visiting);
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
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut FxHashSet<NodeId>,
) -> Option<SpExpr> {
    match *fg.graph.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == sp_vn => Some(SpExpr::Terminal {
            base: out,
            offset: 0,
        }),
        NodeKind::ControlPhi(vn) if vn == sp_vn => {
            decompose_sp_phi(fg, node, sp_vn, memo, visiting)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let l = inputs[0];
            let r = inputs[1];
            if let Some(c) = int_const_signed(fg, r) {
                decompose_sp(fg, l, sp_vn, memo, visiting).map(|e| e.shifted(c))
            } else if let Some(c) = int_const_signed(fg, l) {
                decompose_sp(fg, r, sp_vn, memo, visiting).map(|e| e.shifted(c))
            } else {
                None
            }
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Sub) => {
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() != 2 {
                return None;
            }
            let l = inputs[0];
            let r = inputs[1];
            int_const_signed(fg, r).and_then(|c| {
                decompose_sp(fg, l, sp_vn, memo, visiting).map(|e| e.shifted(c.wrapping_neg()))
            })
        }
        _ => None,
    }
}

fn decompose_sp_phi(
    fg: &BuiltFunctionGraph,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut FxHashSet<NodeId>,
) -> Option<SpExpr> {
    let inputs = fg.graph.node_inputs(node);
    // A ControlPhi has inputs[0] = dispatch token, inputs[1..] = per-pred
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
            decompose_sp(fg, pred_input, sp_vn, memo, visiting)?
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
            addr: rsleigh::VnAddr { space: rsleigh::VnSpace::REGISTER, off: 0x20 },
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
    fn int_const_signed_u32_negative() -> crate::Result<()> {
        // 0xFFFF_FFFC at U32 must read as -4 signed.
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let v = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
        b.build_return(Some(v), &[])?;
        let fg = b.build()?;
        assert_eq!(int_const_signed(&fg, v), Some(-4));
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
        // sp_val is a ControlPhi-of-InitialVar; the phi has 1 predecessor →
        // collapses to Terminal{base: InitialVar(sp), offset: 0}.
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg, sp_val, sp, &mut memo, &mut visiting);
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
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg, addr, sp, &mut memo, &mut visiting);
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
        let neg_four = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg, addr, sp, &mut memo, &mut visiting);
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
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.build_return(Some(addr), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let r1 = {
            let mut v = FxHashSet::default();
            decompose_sp(&fg, addr, sp, &mut memo, &mut v)
        };
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = {
            let mut v = FxHashSet::default();
            decompose_sp(&fg, addr, sp, &mut memo, &mut v)
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
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let c = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_return(Some(c), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        assert!(decompose_sp(&fg, c, sp, &mut memo, &mut visiting).is_none());
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
        let four = b.build_int_const(4, NodeOutputType::U32);
        let eight = b.build_int_const(8, NodeOutputType::U32);
        let twelve = b.build_int_const(12, NodeOutputType::U32);
        let s1 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        let s2 = b.build_int_binary_operation(s1, eight, IntBinaryOp::Sub, NodeOutputType::U32)?;
        let s3 =
            b.build_int_binary_operation(s2, twelve, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.build_return(Some(s3), &[])?;
        let fg = b.build()?;

        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg, s3, sp, &mut memo, &mut visiting);
        assert!(matches!(r, Some(SpExpr::Terminal { offset: -24, .. })));

        // After one top-level walk, all three intermediate outputs must be
        // memoized. (sp_val itself is cached too, but its NodeOutputId is
        // ControlPhi-of-InitialVar, which we don't directly check here.)
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
        let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let c = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_return(Some(c), &[])?;
        let fg = b.build()?;
        let mut memo = SpExprMemo::default();
        let mut visiting = FxHashSet::default();
        let r = decompose_sp(&fg, c, sp, &mut memo, &mut visiting);
        assert!(r.is_none());
        assert!(
            !memo.contains_key(&c),
            "decompose_sp must not cache None verdicts (cycle-truncation cannot be distinguished from genuine 'not SP-rooted' here)"
        );
        Ok(())
    }

    #[test]
    fn decompose_sp_phi_with_non_sp_pred_returns_none() -> crate::Result<()> {
        // A ControlPhi(sp) whose predecessor value is NOT SP-rooted must
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
        let four = b.build_int_const(4, NodeOutputType::U32);
        let sp_minus_4 =
            b.build_int_binary_operation(sp_a, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: sp = 0xDEAD_BEEF (NOT SP-rooted — a literal value pretending
        // to be a new SP).
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEF, NodeOutputType::U32);
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
        let r = decompose_sp(&fg, sp_at_c, sp, &mut memo, &mut visiting);
        assert!(
            r.is_none(),
            "expected None for ControlPhi(sp) with a non-SP-rooted predecessor, got {r:?}"
        );
        Ok(())
    }
}
