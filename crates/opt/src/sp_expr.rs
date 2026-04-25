//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`stack_store::detect`, `stack_load_forward`, `function_args::stack_args`).
//!
//! `decompose_sp` is the workhorse: given an output that may be `InitialVar(sp)`
//! transformed by `Add`/`Sub` of constants and joined by `ControlPhi(sp)`, it
//! returns either a `Terminal { base, offset }` or a `Phi { node, offsets[] }`.
//! Callers thread a per-pass-call memo through it so repeated walks over the
//! same SP chain cost O(1) on cache hit.

use rustc_hash::FxHashMap;
use std::collections::HashSet;

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
    visiting: &mut HashSet<NodeId>,
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
    // Only cache when no cycle was hit on this call path. Approximation:
    // visiting empty here means we returned cleanly.
    if visiting.is_empty() {
        memo.insert(out, result.clone());
    }
    result
}

fn decompose_sp_inner(
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut HashSet<NodeId>,
) -> Option<SpExpr> {
    match *fg.graph.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == sp_vn => Some(SpExpr::Terminal {
            base: out,
            offset: 0,
        }),
        NodeKind::ControlPhi(vn) if vn == sp_vn => {
            decompose_sp_phi(fg, out, node, sp_vn, memo, visiting)
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
    out: NodeOutputId,
    node: NodeId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visiting: &mut HashSet<NodeId>,
) -> Option<SpExpr> {
    let inputs = fg.graph.node_inputs(node);
    if inputs.len() < 2 {
        return Some(SpExpr::Terminal { base: out, offset: 0 });
    }
    let mut offsets = Vec::with_capacity(inputs.len() - 1);
    let mut bases = Vec::with_capacity(inputs.len() - 1);
    for pred_input in inputs.into_iter().skip(1) {
        match decompose_sp(fg, pred_input, sp_vn, memo, visiting) {
            Some(SpExpr::Terminal { base, offset }) => {
                bases.push(base);
                offsets.push(offset);
            }
            _ => return Some(SpExpr::Terminal { base: out, offset: 0 }),
        }
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
        let mut visiting = std::collections::HashSet::new();
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
        let mut visiting = std::collections::HashSet::new();
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
        let mut visiting = std::collections::HashSet::new();
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
            let mut v = std::collections::HashSet::new();
            decompose_sp(&fg, addr, sp, &mut memo, &mut v)
        };
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = {
            let mut v = std::collections::HashSet::new();
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
        let mut visiting = std::collections::HashSet::new();
        assert!(decompose_sp(&fg, c, sp, &mut memo, &mut visiting).is_none());
        Ok(())
    }
}
