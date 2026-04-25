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
