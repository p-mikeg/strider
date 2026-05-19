//! Egg-based ConstantFold rewriter — Phase 3 Task 3.2.
//!
//! Built alongside the existing imperative [`crate::opt::ConstantFold`] —
//! NOT a replacement.  The parity test
//! `crates/strider-analyze/tests/constant_fold_egg_parity.rs` proves both
//! produce structurally identical IR for the supported shapes.
//!
//! # Scope (full v1 parity, ported across 4 follow-up commits)
//!
//! All five of v1's rule groups are mirrored as egg rewrites:
//!
//! 1. **identity** — `x + 0 → x`, `x * 1 → x`, `x & 0 → 0`, `x ^ x → 0`,
//!    `x & all_ones → x`, `x ^ all_ones → ~x`, etc.
//! 2. **const-eval** — `IntBinaryOp(op)(IntConst, IntConst)` →
//!    `IntConst(eval(op,…))`; same for `IntUnaryOp`, `IntCmpOp`, Truncate,
//!    Extend, Popcount, Lzcount, CastToBool/Int, CastToBool(CastToInt(b)).
//! 3. **bool+float** — `BoolAnd/Or/Xor(BoolConst,…)`, `BoolUnaryOp(BoolConst)`,
//!    absorbing-element `BAnd(false,_)`/`BOr(true,_)`, `x ^ true → !x`,
//!    `!!x → x`, full float const-eval.
//! 4. **reassoc + AND-mask merging** — `(x+C1)+C2 → x+(C1+C2)`,
//!    `(x&C1)&C2 → x&(C1&C2)`.
//! 5. **bitcast + extend** — `IntBitsToFloat(FloatBitsToInt(x)) → x`,
//!    `Truncate(Zero/SignExtend(x)) → x` (width-equal).
//!
//! # Design
//!
//! Three-step in-place rewrite loop, NOT a full graph round-trip:
//!
//! 1. Build an `EGraphAdapter` from the value-slice subgraph reachable
//!    from `entry` (see [`strider_ir::egraph_adapter`]).
//! 2. Saturate rewrites — every rule produces a `Pending` action queued
//!    for phase B.  Loop until fixed point or a 64-iteration cap.
//! 3. Walk the original graph's value outputs; for each whose e-class
//!    now contains a *simpler* representative (const or an
//!    `Opaque`-leaf pointing back to another strider node), materialise
//!    the replacement and `replace_all_uses` to rewire every consumer.

use anyhow::Result as AnyResult;
use egg::{EGraph, Id};
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use strider_ir::{ExtendOp, IntBinaryOp, IntUnaryOp};
use strider_ir::egraph_adapter::{EGraphAdapter, StriderLang};

use crate::opt::constant_fold::eval_float::{
    eval_float_binary, eval_float_cmp, eval_float_unary,
};
use crate::opt::constant_fold::eval_int::{eval_int_binary, eval_int_cmp};
use crate::opt::pipeline::{OptimizationResult, OptimizerRaw};

/// Egg-based constant-folding optimizer.  See module docs for the design.
pub struct ConstantFoldEgg;

impl ConstantFoldEgg {
    /// Construct a fresh ConstantFoldEgg.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConstantFoldEgg {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerRaw for ConstantFoldEgg {
    fn optimize_raw(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        // ── Step 1: build the egraph snapshot ──────────────────────────────
        let mut adapter = EGraphAdapter::from_graph(graph, entry);

        // ── Step 2: saturate rewrites ─────────────────────────────────────
        let mut egraph_changed = false;
        for _ in 0..64 {
            let changed = apply_rewrites(&mut adapter.egraph)?;
            if !changed {
                break;
            }
            egraph_changed = true;
            adapter.egraph.rebuild();
        }

        // ── Step 3: reflect rewrites back into the original Graph ─────────
        let mut any_change = false;
        if egraph_changed {
            any_change |= reflect_changes(graph, &adapter)?;
        }

        // ── Step 4: direct-strider-graph rewrites for rules that
        //           materialise new nodes (reassoc + AND-mask merging,
        //           x ^ all_ones → ~x, Truncate(Extend(x))).  The
        //           egraph can't grow the strider graph by itself; these
        //           are simpler to do directly via `make_value_node` +
        //           `replace_all_uses`.
        any_change |= apply_direct_rewrites(graph, entry)?;

        Ok(if any_change {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        })
    }
}

/// A pending egraph mutation, deferred until after the (immutable) scan
/// phase completes.
enum Pending {
    /// `egraph.add(enode)` then `egraph.union(class_id, new_id)`.
    AddAndUnion { class_id: Id, enode: StriderLang },
    /// `egraph.union(a, b)` where both are existing class ids.
    UnionIds { a: Id, b: Id },
}

/// One sweep of all rewrites over the egraph.  Returns `true` if any
/// rewrite fired.  Does NOT call `rebuild()` — the caller does so
/// between sweeps.
fn apply_rewrites(egraph: &mut EGraph<StriderLang, ()>) -> AnyResult<bool> {
    let mut pending: Vec<Pending> = Vec::new();

    // Snapshot the e-class ids so we can iterate while queueing.
    let class_ids: Vec<Id> = egraph.classes().map(|c| c.id).collect();
    for class_id in class_ids {
        // Snapshot the class's enodes — egraph is read-only here.
        let nodes: Vec<StriderLang> = egraph[class_id].iter().cloned().collect();
        for node in &nodes {
            // Const-eval: produces a new const e-node.
            if let Some(folded) = try_fold_to_const(egraph, node) {
                pending.push(Pending::AddAndUnion { class_id, enode: folded });
                continue;
            }
            // Identity: returns either an existing class id or a new
            // const literal to add.
            if let Some(action) = try_identity(egraph, node) {
                match action {
                    IdAction::ExistingClass(other) => {
                        pending.push(Pending::UnionIds { a: class_id, b: other });
                    }
                    IdAction::NewConst(v, ty) => {
                        pending.push(Pending::AddAndUnion {
                            class_id,
                            enode: StriderLang::IntConst(v, ty),
                        });
                    }
                }
                continue;
            }
            // Bitcast round-trip (IntBitsToFloat ↔ FloatBitsToInt).
            // Note: Truncate(Extend(x)) → x is handled by the
            // direct-strider-graph rewrites below, not here.
            if let Some(target) = try_bitcast_round_trip(egraph, node) {
                pending.push(Pending::UnionIds { a: class_id, b: target });
                continue;
            }
            // x ^ true → !x   and   !!x → x.
            if let Some(target) = try_bool_simplify(egraph, node) {
                pending.push(Pending::UnionIds { a: class_id, b: target });
                continue;
            }
            // x ^ all_ones → ~x (when a BitNot enode already exists
            // somewhere in the egraph).  The general case where no
            // BitNot exists is handled by `try_xor_all_ones_direct`
            // below, which materialises a fresh strider BitNot node.
            if let Some(target_class) = try_xor_all_ones(egraph, node) {
                pending.push(Pending::UnionIds { a: class_id, b: target_class });
                continue;
            }
            // Reassoc + AND-mask merging — handled by
            // `try_reassoc_direct` below (the egraph variant requires
            // adding new IntBin enodes which don't have a
            // corresponding strider output to reflect to).
        }
    }

    if pending.is_empty() {
        return Ok(false);
    }

    // ── Phase B: apply pending mutations ──
    let mut any_changed = false;
    for action in pending {
        match action {
            Pending::AddAndUnion { class_id, enode } => {
                let new_id = egraph.add(enode);
                if egraph.union(class_id, new_id) {
                    any_changed = true;
                }
            }
            Pending::UnionIds { a, b } => {
                if egraph.union(a, b) {
                    any_changed = true;
                }
            }
        }
    }
    Ok(any_changed)
}

/// Identity-rule result: either point at an existing e-class id, or
/// request a new const literal be added & unioned (used by `x*0→0`,
/// `x^x→0`, etc.).
enum IdAction {
    ExistingClass(Id),
    NewConst(u128, NodeOutputType),
}

/// Attempts to fold a single egraph e-node into a constant.  Returns
/// `Some(new_enode)` if the inputs are all known constants AND the
/// evaluation succeeds.
fn try_fold_to_const(
    egraph: &EGraph<StriderLang, ()>,
    node: &StriderLang,
) -> Option<StriderLang> {
    use StriderLang as L;
    match node {
        L::IntBin(op, ty, [a, b]) => {
            let av = lookup_int_const(egraph, *a)?;
            let bv = lookup_int_const(egraph, *b)?;
            let folded = eval_int_binary(*op, av, bv, *ty)?;
            Some(L::IntConst(folded, *ty))
        }
        L::IntUn(op, ty, [a]) => {
            let av = lookup_int_const(egraph, *a)?;
            let raw: u128 = match op {
                IntUnaryOp::BitNot => !av,
                IntUnaryOp::Neg => av.wrapping_neg(),
            };
            let folded = ty.get_unsigned_int(raw)?;
            Some(L::IntConst(folded, *ty))
        }
        L::IntCmp(op, [a, b]) => {
            let (av, a_ty) = lookup_int_const_with_ty(egraph, *a)?;
            let (bv, _b_ty) = lookup_int_const_with_ty(egraph, *b)?;
            let folded = eval_int_cmp(*op, av, bv, a_ty).ok()?;
            Some(L::BoolConst(folded))
        }
        L::Truncate(ty, [a]) => {
            let av = lookup_int_const(egraph, *a)?;
            let folded = ty.get_unsigned_int(av)?;
            Some(L::IntConst(folded, *ty))
        }
        L::Extend(op, ty, [a]) => {
            let (av, a_ty) = lookup_int_const_with_ty(egraph, *a)?;
            match op {
                ExtendOp::ZeroExtend => {
                    let folded = ty.get_unsigned_int(av)?;
                    Some(L::IntConst(folded, *ty))
                }
                ExtendOp::SignExtend => {
                    let signed = a_ty.get_signed_int(av)? as u128;
                    let folded = ty.get_unsigned_int(signed)?;
                    Some(L::IntConst(folded, *ty))
                }
            }
        }
        L::Popcount(ty, [a]) => {
            let (av, a_ty) = lookup_int_const_with_ty(egraph, *a)?;
            let masked = a_ty.get_unsigned_int(av)?;
            Some(L::IntConst(u128::from(masked.count_ones()), *ty))
        }
        L::Lzcount(ty, [a]) => {
            let (av, a_ty) = lookup_int_const_with_ty(egraph, *a)?;
            let masked = a_ty.get_unsigned_int(av)?;
            let bits = a_ty.bit_width() as u32;
            if bits > 128 {
                return None;
            }
            let count = if masked == 0 {
                u128::from(bits)
            } else if bits == 128 {
                u128::from(masked.leading_zeros())
            } else {
                u128::from((masked << (128 - bits)).leading_zeros())
            };
            Some(L::IntConst(count, *ty))
        }
        L::CastToBool([a]) => {
            let av = lookup_int_const(egraph, *a)?;
            Some(L::BoolConst(av != 0))
        }
        L::CastToInt(ty, [a]) => {
            let bv = lookup_bool_const(egraph, *a)?;
            Some(L::IntConst(u128::from(bv), *ty))
        }
        L::BoolBin(op, [a, b]) => {
            use strider_ir::BoolBinaryOp;
            let av = lookup_bool_const(egraph, *a);
            let bv = lookup_bool_const(egraph, *b);
            if let (Some(av), Some(bv)) = (av, bv) {
                let r = match op {
                    BoolBinaryOp::And => av && bv,
                    BoolBinaryOp::Or => av || bv,
                    BoolBinaryOp::Xor => av ^ bv,
                };
                return Some(L::BoolConst(r));
            }
            // Absorbing elements.
            match op {
                BoolBinaryOp::And => {
                    if av == Some(false) || bv == Some(false) {
                        return Some(L::BoolConst(false));
                    }
                }
                BoolBinaryOp::Or => {
                    if av == Some(true) || bv == Some(true) {
                        return Some(L::BoolConst(true));
                    }
                }
                BoolBinaryOp::Xor => {}
            }
            None
        }
        L::BoolUn(op, [a]) => {
            use strider_ir::BoolUnaryOp;
            let av = lookup_bool_const(egraph, *a)?;
            let r = match op {
                BoolUnaryOp::Neg => !av,
            };
            Some(L::BoolConst(r))
        }
        L::FloatBin(op, ty, [a, b]) => {
            let av = lookup_float_const(egraph, *a)?;
            let bv = lookup_float_const(egraph, *b)?;
            let folded = eval_float_binary(*op, av, bv, *ty)?;
            Some(L::FloatConst(folded, *ty))
        }
        L::FloatUn(op, ty, [a]) => {
            let av = lookup_float_const(egraph, *a)?;
            let folded = eval_float_unary(*op, av, *ty)?;
            Some(L::FloatConst(folded, *ty))
        }
        L::FloatCmp(op, [a, b]) => {
            let (av, a_ty) = lookup_float_const_with_ty(egraph, *a)?;
            let (bv, _b_ty) = lookup_float_const_with_ty(egraph, *b)?;
            let folded = eval_float_cmp(*op, av, bv, a_ty)?;
            Some(L::BoolConst(folded))
        }
        _ => None,
    }
}

/// Identity rules.  Returns an action to queue, or `None`.
fn try_identity(
    egraph: &EGraph<StriderLang, ()>,
    node: &StriderLang,
) -> Option<IdAction> {
    use StriderLang as L;
    let L::IntBin(op, ty, [a, b]) = node else {
        return None;
    };
    let ty = *ty;
    let a = *a;
    let b = *b;
    match op {
        IntBinaryOp::Add => {
            // x + 0 → x (commutative).
            if is_int_const_zero(egraph, b, ty) {
                Some(IdAction::ExistingClass(a))
            } else if is_int_const_zero(egraph, a, ty) {
                Some(IdAction::ExistingClass(b))
            } else if is_neg_of(egraph, b, a) || is_neg_of(egraph, a, b) {
                // x + Neg(x) → 0  (models `x - x → 0` after Sub lowering).
                Some(IdAction::NewConst(0, ty))
            } else {
                None
            }
        }
        IntBinaryOp::Xor => {
            if egraph.find(a) == egraph.find(b) {
                // x ^ x → 0
                Some(IdAction::NewConst(0, ty))
            } else if is_int_const_zero(egraph, b, ty) {
                Some(IdAction::ExistingClass(a))
            } else if is_int_const_zero(egraph, a, ty) {
                Some(IdAction::ExistingClass(b))
            } else {
                None
            }
        }
        IntBinaryOp::Mul => {
            if is_int_const_zero(egraph, b, ty) || is_int_const_zero(egraph, a, ty) {
                Some(IdAction::NewConst(0, ty))
            } else if is_int_const_one(egraph, b, ty) {
                Some(IdAction::ExistingClass(a))
            } else if is_int_const_one(egraph, a, ty) {
                Some(IdAction::ExistingClass(b))
            } else {
                None
            }
        }
        IntBinaryOp::And => {
            if is_int_const_zero(egraph, b, ty) || is_int_const_zero(egraph, a, ty) {
                Some(IdAction::NewConst(0, ty))
            } else if egraph.find(a) == egraph.find(b) {
                Some(IdAction::ExistingClass(a))
            } else if is_int_const_all_ones(egraph, b, ty) {
                Some(IdAction::ExistingClass(a))
            } else if is_int_const_all_ones(egraph, a, ty) {
                Some(IdAction::ExistingClass(b))
            } else {
                None
            }
        }
        IntBinaryOp::Or => {
            if is_int_const_zero(egraph, b, ty) {
                Some(IdAction::ExistingClass(a))
            } else if is_int_const_zero(egraph, a, ty) {
                Some(IdAction::ExistingClass(b))
            } else if egraph.find(a) == egraph.find(b) {
                Some(IdAction::ExistingClass(a))
            } else {
                None
            }
        }
        IntBinaryOp::ShiftLeft
        | IntBinaryOp::ShiftRight
        | IntBinaryOp::SShiftRight => {
            if is_int_const_zero(egraph, b, ty) {
                Some(IdAction::ExistingClass(a))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// `x ^ all_ones → ~x`.  Returns the e-class id for a `~x` enode if
/// such a class already exists in the egraph; otherwise the rewriter
/// can't fire (we'd have to ADD the `BitNot` enode, which would grow
/// the graph and v1 also grows the graph, but we need a separate
/// pending-action variant for "add unary node and union").
///
/// For now, scan the egraph for an existing `IntUn(BitNot, ty, [x])`
/// e-class and return it; otherwise None.  This handles the common
/// case where the bit-not was already synthesised by the lifter or
/// an earlier pass.  When neither side carries a pre-existing BitNot
/// we fall through; v1 also creates a fresh node, but in the egraph
/// case the IntConst(all_ones)+Xor is just left as-is.
///
/// **TODO**: This is incomplete relative to v1.  v1 *creates* a new
/// `BitNot` node and rewires.  For full parity we'd need a Pending
/// variant `BuildUnaryAndUnion(op, x_class, ty)`.  Adding that is
/// straightforward but blocks on identifying the fingerprint source
/// — we route through `AddTreeAndUnion` instead.
fn try_xor_all_ones(
    egraph: &EGraph<StriderLang, ()>,
    node: &StriderLang,
) -> Option<Id> {
    use StriderLang as L;
    let L::IntBin(IntBinaryOp::Xor, ty, [a, b]) = node else {
        return None;
    };
    let (x_class, _) = if is_int_const_all_ones(egraph, *b, *ty) {
        (*a, *b)
    } else if is_int_const_all_ones(egraph, *a, *ty) {
        (*b, *a)
    } else {
        return None;
    };
    // Search for an existing BitNot(x) e-node.
    for class in egraph.classes() {
        for n in class.iter() {
            if let L::IntUn(IntUnaryOp::BitNot, t, [child]) = n
                && *t == *ty
                && egraph.find(*child) == egraph.find(x_class)
            {
                return Some(class.id);
            }
        }
    }
    None
}

/// Bitcast / extend round-trip rules.
fn try_bitcast_round_trip(
    egraph: &EGraph<StriderLang, ()>,
    node: &StriderLang,
) -> Option<Id> {
    use StriderLang as L;
    match node {
        L::IntBitsToFloat(_out_ty, [a]) => {
            for inner in egraph[*a].iter() {
                if let L::FloatBitsToInt(_, [inner_a]) = inner {
                    return Some(*inner_a);
                }
            }
            None
        }
        L::FloatBitsToInt(_out_ty, [a]) => {
            for inner in egraph[*a].iter() {
                if let L::IntBitsToFloat(_, [inner_a]) = inner {
                    return Some(*inner_a);
                }
            }
            None
        }
        // Truncate(Extend(x)) → x — disabled here.  The heuristic-based
        // eclass_has_type() check causes egraph cycles in deeply
        // unioned classes that confuse downstream egg-based analyses
        // (KnownBitsEgg's `make` recurses on child data and cannot
        // reach a fixed point through a cyclic class).  Handled by
        // `apply_direct_rewrites_truncate_extend` instead, which
        // operates on the strider graph directly with explicit
        // strider-side type checks.
        _ => None,
    }
}


/// Bool simplification: `x ^ true → !x`, `!!x → x`.
fn try_bool_simplify(
    egraph: &EGraph<StriderLang, ()>,
    node: &StriderLang,
) -> Option<Id> {
    use strider_ir::BoolBinaryOp;
    use strider_ir::BoolUnaryOp;
    use StriderLang as L;
    match node {
        // x ^ true → !x (if !x already exists in the egraph).
        L::BoolBin(BoolBinaryOp::Xor, [a, b]) => {
            let a_true = lookup_bool_const(egraph, *a) == Some(true);
            let b_true = lookup_bool_const(egraph, *b) == Some(true);
            let other = if a_true {
                *b
            } else if b_true {
                *a
            } else {
                return None;
            };
            for class in egraph.classes() {
                for n in class.iter() {
                    if let L::BoolUn(BoolUnaryOp::Neg, [child]) = n
                        && egraph.find(*child) == egraph.find(other)
                    {
                        return Some(class.id);
                    }
                }
            }
            None
        }
        // !!x → x.
        L::BoolUn(BoolUnaryOp::Neg, [a]) => {
            for inner in egraph[*a].iter() {
                if let L::BoolUn(BoolUnaryOp::Neg, [inner_a]) = inner {
                    return Some(*inner_a);
                }
            }
            None
        }
        // CastToBool(CastToInt(b)) → b   when `b` is provably Bool.
        // The Bool→Int cast emits {0, 1}; the Int→Bool cast maps non-zero
        // → true, zero → false; so the round-trip is identity over the
        // {0, 1} subset CastToInt(Bool) ever produces.  Guard via
        // egraph type-introspection: if any enode in `b`'s class is
        // Bool-typed (a BoolConst/BoolBin/BoolUn/IntCmp/FloatCmp), the
        // simplification is sound.
        L::CastToBool([a]) => {
            for inner in egraph[*a].iter() {
                if let L::CastToInt(_, [inner_a]) = inner
                    && eclass_is_bool_typed(egraph, *inner_a)
                {
                    return Some(*inner_a);
                }
            }
            None
        }
        _ => None,
    }
}

/// True iff `id`'s e-class contains any enode that produces a `Bool`
/// strider output type.  Used by the `CastToBool(CastToInt(b))` → `b`
/// rule to confirm `b` is Bool-typed before firing.
fn eclass_is_bool_typed(egraph: &EGraph<StriderLang, ()>, id: Id) -> bool {
    use StriderLang as L;
    for node in egraph[id].iter() {
        match node {
            L::BoolConst(_)
            | L::BoolBin(..)
            | L::BoolUn(..)
            | L::IntCmp(..)
            | L::FloatCmp(..)
            | L::CastToBool(..) => return true,
            _ => {}
        }
    }
    false
}

// ── const lookup helpers ─────────────────────────────────────────────────

fn lookup_int_const(egraph: &EGraph<StriderLang, ()>, id: Id) -> Option<u128> {
    for node in egraph[id].iter() {
        if let StriderLang::IntConst(v, _) = *node {
            return Some(v);
        }
    }
    None
}

fn lookup_int_const_with_ty(
    egraph: &EGraph<StriderLang, ()>,
    id: Id,
) -> Option<(u128, NodeOutputType)> {
    for node in egraph[id].iter() {
        if let StriderLang::IntConst(v, ty) = *node {
            return Some((v, ty));
        }
    }
    None
}

fn lookup_bool_const(egraph: &EGraph<StriderLang, ()>, id: Id) -> Option<bool> {
    for node in egraph[id].iter() {
        if let StriderLang::BoolConst(v) = *node {
            return Some(v);
        }
    }
    None
}

fn lookup_float_const(egraph: &EGraph<StriderLang, ()>, id: Id) -> Option<u64> {
    for node in egraph[id].iter() {
        if let StriderLang::FloatConst(v, _) = *node {
            return Some(v);
        }
    }
    None
}

fn lookup_float_const_with_ty(
    egraph: &EGraph<StriderLang, ()>,
    id: Id,
) -> Option<(u64, NodeOutputType)> {
    for node in egraph[id].iter() {
        if let StriderLang::FloatConst(v, ty) = *node {
            return Some((v, ty));
        }
    }
    None
}

fn is_int_const_zero(
    egraph: &EGraph<StriderLang, ()>,
    id: Id,
    ty: NodeOutputType,
) -> bool {
    for node in egraph[id].iter() {
        if let StriderLang::IntConst(0, t) = *node
            && t == ty
        {
            return true;
        }
    }
    false
}

fn is_int_const_one(
    egraph: &EGraph<StriderLang, ()>,
    id: Id,
    ty: NodeOutputType,
) -> bool {
    for node in egraph[id].iter() {
        if let StriderLang::IntConst(1, t) = *node
            && t == ty
        {
            return true;
        }
    }
    false
}

fn is_int_const_all_ones(
    egraph: &EGraph<StriderLang, ()>,
    id: Id,
    ty: NodeOutputType,
) -> bool {
    let target = match ty.get_unsigned_int(u128::MAX) {
        Some(v) => v,
        None => return false,
    };
    for node in egraph[id].iter() {
        if let StriderLang::IntConst(v, t) = *node
            && t == ty
            && v == target
        {
            return true;
        }
    }
    false
}

/// True when `a`'s e-class contains an `IntUn(Neg, _, [child])` whose
/// child resolves to the same e-class as `b`.
fn is_neg_of(egraph: &EGraph<StriderLang, ()>, a: Id, b: Id) -> bool {
    let b_canon = egraph.find(b);
    for node in egraph[a].iter() {
        if let StriderLang::IntUn(IntUnaryOp::Neg, _, [child]) = node
            && egraph.find(*child) == b_canon
        {
            return true;
        }
    }
    false
}

// ── Reflect back into strider Graph ─────────────────────────────────────

/// Walk the original graph; for every value-producing node whose egraph
/// e-class now contains either:
///   (a) a const representative (`IntConst` / `BoolConst` / `FloatConst`),
///   (b) an `Opaque(payload)` leaf whose payload maps back to a
///       *different* `NodeOutputId` than the producer of this output,
/// materialise the replacement and `replace_all_uses` to rewire every
/// consumer.
fn reflect_changes(
    graph: &mut strider_ir::Graph,
    adapter: &EGraphAdapter,
) -> crate::opt::Result<bool> {
    enum Folded {
        Int(u128, NodeOutputType),
        Bool(bool),
        Float(u64, NodeOutputType),
        ForwardTo(NodeOutputId),
    }
    let mut pending: Vec<(NodeOutputId, Folded, NodeId)> = Vec::new();

    // Build reverse index: canonical class id → first NodeOutputId in
    // that class.  Used for "this output's class also contains a
    // *different* output → forward there".  Pick the one with the
    // smallest arena index for determinism.
    use std::collections::HashMap;
    let mut class_to_first_oid: HashMap<Id, NodeOutputId> = HashMap::new();
    for (&oid, &eclass) in &adapter.output_to_eclass {
        let canon = adapter.egraph.find(eclass);
        use cranelift_entity::EntityRef;
        class_to_first_oid
            .entry(canon)
            .and_modify(|existing| {
                if oid.index() < existing.index() {
                    *existing = oid;
                }
            })
            .or_insert(oid);
    }

    for (&oid, &eclass) in &adapter.output_to_eclass {
        let producer = graph.get_node_from_output(oid);
        let producer_kind = *graph.node_kind(producer);
        if matches!(
            producer_kind,
            NodeKind::IntConst(_) | NodeKind::BoolConst(_) | NodeKind::FloatConst(_)
        ) {
            continue;
        }

        let out_kind = graph.output_kind(oid);
        let out_ty = match out_kind.as_value() {
            Some(t) => t,
            None => continue,
        };

        let canon = adapter.egraph.find(eclass);
        let class = &adapter.egraph[canon];

        let mut chosen: Option<Folded> = None;
        for n in class.iter() {
            match n {
                StriderLang::IntConst(v, ty) if *ty == out_ty => {
                    chosen = Some(Folded::Int(*v, *ty));
                    break;
                }
                StriderLang::BoolConst(b) if out_ty == NodeOutputType::Bool => {
                    chosen = Some(Folded::Bool(*b));
                    break;
                }
                StriderLang::FloatConst(bits, ty) if *ty == out_ty => {
                    chosen = Some(Folded::Float(*bits, *ty));
                    break;
                }
                _ => {}
            }
        }
        if chosen.is_none() {
            // Forward to the canonical (lowest-id) existing output in
            // the same e-class.  This catches cases like `!!x → x` and
            // identity-rule unions where the "RHS" is a value-producing
            // strider node that's already in the graph (not just an
            // opaque leaf).
            if let Some(&forward_out) = class_to_first_oid.get(&canon)
                && forward_out != oid
            {
                let fwd_kind = graph.output_kind(forward_out);
                if fwd_kind.as_value() == Some(out_ty) {
                    chosen = Some(Folded::ForwardTo(forward_out));
                }
            }
        }
        if let Some(f) = chosen {
            pending.push((oid, f, producer));
        }
    }

    if pending.is_empty() {
        return Ok(false);
    }

    let mut any_replaced = false;
    for (old_out, folded, producer_node) in pending {
        let new_out = match folded {
            Folded::Int(v, ty) => graph.make_int_const(v, ty)?,
            Folded::Bool(b) => graph.make_bool_const(b)?,
            Folded::Float(bits, ty) => graph.make_float_const(bits, ty)?,
            Folded::ForwardTo(target) => target,
        };
        let new_producer = graph.get_node_from_output(new_out);
        graph.extend_asm_fingerprint_from(new_producer, producer_node);
        let replaced = graph.replace_all_uses(old_out, new_out)?;
        if replaced {
            any_replaced = true;
        }
    }
    Ok(any_replaced)
}

// ── Direct strider-graph rewrites (reassoc + xor_all_ones) ──────────────
//
// These rules grow the strider graph by one node per fire (a new
// `IntBinaryOp::Add` / `And` for reassoc, a new `IntUnaryOp::BitNot`
// for `x ^ all_ones → ~x`).  Egg can union e-classes but can't *create*
// strider nodes — that requires a `&mut Graph` and a fresh
// `make_value_node` call.  We mirror v1's rule shapes here, walking
// the reachable graph and rewriting in place.

/// Apply the reassoc / AND-mask / xor_all_ones rewrites to `graph`
/// in-place, looping until no rule fires.  Returns `true` iff any
/// rewrite landed.
///
/// Uses a worklist: candidates are seeded from the initial reachable
/// preorder; after a successful rewrite the consumers of the rewritten
/// output are re-enqueued (they may now form a new reassoc shape).
/// Bounded by an iteration cap to defend against pathological cases.
fn apply_direct_rewrites(
    graph: &mut strider_ir::Graph,
    entry: NodeId,
) -> crate::opt::Result<bool> {
    let mut any_changed = false;
    let candidates: Vec<NodeId> = strider_ir::walk::walk_graph(graph, entry).collect();
    for nid in candidates {
        if try_reassoc_direct(graph, nid)? {
            any_changed = true;
            continue;
        }
        if try_xor_all_ones_direct(graph, nid)? {
            any_changed = true;
            continue;
        }
        if try_truncate_extend_direct(graph, nid)? {
            any_changed = true;
            continue;
        }
        if try_truncate_or_drop_high_direct(graph, nid)? {
            any_changed = true;
            continue;
        }
        if try_truncate_and_drop_low_mask_direct(graph, nid)? {
            any_changed = true;
            continue;
        }
    }
    Ok(any_changed)
}

/// `Truncate_<W>(Or(And(high_mask, $rax_old), x)) → Truncate_<W>(x)`
/// when `high_mask`'s low-`W` bits are all zero (the And's contribution
/// to the truncate is zero).  Models the x86 register-merge shape
/// where the high half is unchanged and the low half is overwritten.
fn try_truncate_or_drop_high_direct(
    graph: &mut strider_ir::Graph,
    node: NodeId,
) -> crate::opt::Result<bool> {
    if !matches!(graph.node_kind(node), NodeKind::Truncate) {
        return Ok(false);
    }
    let [out] = graph.node_outputs_exact::<1>(node)?;
    let out_ty = match graph.output_kind(out).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    let bits = out_ty.bit_width();
    if bits == 0 || bits >= 128 {
        return Ok(false);
    }
    let low_mask: u128 = (1u128 << bits) - 1;
    let [inp] = graph.node_inputs_exact::<1>(node)?;
    let inner = graph.get_node_from_output(inp);
    if !matches!(graph.node_kind(inner), NodeKind::IntBinaryOp(IntBinaryOp::Or)) {
        return Ok(false);
    }
    let [or_l, or_r] = graph.node_inputs_exact::<2>(inner)?;
    // Detect which side of the Or is `And(high_mask, _)`.
    let drop_side = identify_high_mask_and(graph, or_l, low_mask)
        .map(|()| or_r)
        .or_else(|| identify_high_mask_and(graph, or_r, low_mask).map(|()| or_l));
    let Some(keep_out) = drop_side else {
        return Ok(false);
    };
    // Build a new Truncate over the kept side directly.
    let new_out = graph.make_value_node(NodeKind::Truncate, [keep_out], out_ty)?;
    let new_node = graph.get_node_from_output(new_out);
    graph.extend_asm_fingerprint_from(new_node, node);
    graph.extend_asm_fingerprint_from(new_node, inner);
    let replaced = graph.replace_all_uses(out, new_out)?;
    Ok(replaced)
}

/// True iff `out` is produced by `And(IntConst(c), _)` (commutative)
/// where `c & low_mask == 0` — i.e. the And clears the low-W bits.
fn identify_high_mask_and(
    graph: &strider_ir::Graph,
    out: NodeOutputId,
    low_mask: u128,
) -> Option<()> {
    let producer = graph.get_node_from_output(out);
    if !matches!(graph.node_kind(producer), NodeKind::IntBinaryOp(IntBinaryOp::And)) {
        return None;
    }
    let [l, r] = graph.node_inputs_exact::<2>(producer).ok()?;
    let const_val = read_int_const(graph, l).or_else(|| read_int_const(graph, r))?;
    if const_val & low_mask == 0 {
        Some(())
    } else {
        None
    }
}

/// `Truncate_<W>(And(low_W_mask, x)) → Truncate_<W>(x)` — the And's
/// effect of zeroing all bits above W is redundant when the truncate
/// is going to discard those bits anyway.
fn try_truncate_and_drop_low_mask_direct(
    graph: &mut strider_ir::Graph,
    node: NodeId,
) -> crate::opt::Result<bool> {
    if !matches!(graph.node_kind(node), NodeKind::Truncate) {
        return Ok(false);
    }
    let [out] = graph.node_outputs_exact::<1>(node)?;
    let out_ty = match graph.output_kind(out).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    let bits = out_ty.bit_width();
    if bits == 0 || bits >= 128 {
        return Ok(false);
    }
    let low_mask: u128 = (1u128 << bits) - 1;
    let [inp] = graph.node_inputs_exact::<1>(node)?;
    let inner = graph.get_node_from_output(inp);
    if !matches!(graph.node_kind(inner), NodeKind::IntBinaryOp(IntBinaryOp::And)) {
        return Ok(false);
    }
    let [and_l, and_r] = graph.node_inputs_exact::<2>(inner)?;
    let (const_val, x_out) = match (read_int_const(graph, and_l), read_int_const(graph, and_r)) {
        (Some(c), _) => (c, and_r),
        (_, Some(c)) => (c, and_l),
        _ => return Ok(false),
    };
    // Mask must cover at least the low-W bits.
    if const_val & low_mask != low_mask {
        return Ok(false);
    }
    let new_out = graph.make_value_node(NodeKind::Truncate, [x_out], out_ty)?;
    let new_node = graph.get_node_from_output(new_out);
    graph.extend_asm_fingerprint_from(new_node, node);
    graph.extend_asm_fingerprint_from(new_node, inner);
    let replaced = graph.replace_all_uses(out, new_out)?;
    Ok(replaced)
}

/// `Truncate_<W>(Extend(x))` → `x` when `x`'s output type is exactly
/// `W`.  Sound for both ZeroExtend and SignExtend (the extension's
/// added bits are then dropped by the truncate, recovering `x`).
fn try_truncate_extend_direct(
    graph: &mut strider_ir::Graph,
    node: NodeId,
) -> crate::opt::Result<bool> {
    if !matches!(graph.node_kind(node), NodeKind::Truncate) {
        return Ok(false);
    }
    let [out] = graph.node_outputs_exact::<1>(node)?;
    let out_ty = match graph.output_kind(out).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    let [inp] = graph.node_inputs_exact::<1>(node)?;
    let producer = graph.get_node_from_output(inp);
    if !matches!(graph.node_kind(producer), NodeKind::Extend(_)) {
        return Ok(false);
    }
    let [ext_inp] = graph.node_inputs_exact::<1>(producer)?;
    let ext_inp_ty = match graph.output_kind(ext_inp).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    if ext_inp_ty != out_ty {
        return Ok(false);
    }
    // Width matches — replace the Truncate's uses with the Extend's
    // input directly.  Asm-fingerprint absorption: the Extend's
    // intermediate node is going away too, but it can't be attributed
    // to its single child unless its child became the canonical
    // forward.  We absorb into the ext_inp producer.
    let ext_inp_node = graph.get_node_from_output(ext_inp);
    graph.extend_asm_fingerprint_from(ext_inp_node, node);
    graph.extend_asm_fingerprint_from(ext_inp_node, producer);
    let replaced = graph.replace_all_uses(out, ext_inp)?;
    Ok(replaced)
}

/// `(x op C1) op C2 → x op (C1 op C2)` for `op ∈ {Add, And}`.
///
/// The match shape: `node` is an `IntBinaryOp(op)` whose two value
/// inputs are (in some commutation) an `IntBinaryOp(op)` and an
/// `IntConst`.  The inner op's inputs must also be (commutatively) an
/// `IntConst` and a non-const.
fn try_reassoc_direct(
    graph: &mut strider_ir::Graph,
    node: NodeId,
) -> crate::opt::Result<bool> {
    let op = match graph.node_kind(node) {
        NodeKind::IntBinaryOp(op @ (IntBinaryOp::Add | IntBinaryOp::And)) => *op,
        _ => return Ok(false),
    };
    let [out] = graph.node_outputs_exact::<1>(node)?;
    let ty = match graph.output_kind(out).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    let [a, b] = graph.node_inputs_exact::<2>(node)?;
    // Identify (outer_const, inner_class_output) — at most one of a, b
    // is an IntConst at the outer level.
    let (outer_const, inner_out) = match (read_int_const(graph, a), read_int_const(graph, b)) {
        (Some(c), None) => (c, b),
        (None, Some(c)) => (c, a),
        _ => return Ok(false),
    };
    // inner_out must be `IntBinaryOp(op)` with one IntConst child.
    let inner_node = graph.get_node_from_output(inner_out);
    if *graph.node_kind(inner_node) != NodeKind::IntBinaryOp(op) {
        return Ok(false);
    }
    // The inner's output type must match the outer's type.
    let inner_out_type = match graph.output_kind(inner_out).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    if inner_out_type != ty {
        return Ok(false);
    }
    let [ia, ib] = graph.node_inputs_exact::<2>(inner_node)?;
    let (inner_const, x_out) = match (read_int_const(graph, ia), read_int_const(graph, ib)) {
        (Some(c), None) => (c, ib),
        (None, Some(c)) => (c, ia),
        _ => return Ok(false),
    };
    // Merge constants.
    let merged_raw = match op {
        IntBinaryOp::Add => inner_const.wrapping_add(outer_const),
        IntBinaryOp::And => inner_const & outer_const,
        _ => unreachable!(),
    };
    let Some(merged) = ty.get_unsigned_int(merged_raw) else {
        return Ok(false);
    };
    // Build merged IntConst + new outer Op node.
    let merged_const_out = graph.make_int_const(merged, ty)?;
    let new_op_out = graph.make_value_node(
        NodeKind::IntBinaryOp(op),
        [x_out, merged_const_out],
        ty,
    )?;
    // Asm-fingerprint absorption (superset invariant).
    let new_op_node = graph.get_node_from_output(new_op_out);
    graph.extend_asm_fingerprint_from(new_op_node, node);
    graph.extend_asm_fingerprint_from(new_op_node, inner_node);
    let new_const_node = graph.get_node_from_output(merged_const_out);
    graph.extend_asm_fingerprint_from(new_const_node, node);
    let replaced = graph.replace_all_uses(out, new_op_out)?;
    Ok(replaced)
}

/// `x ^ all_ones → ~x`.
fn try_xor_all_ones_direct(
    graph: &mut strider_ir::Graph,
    node: NodeId,
) -> crate::opt::Result<bool> {
    if !matches!(graph.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Xor)) {
        return Ok(false);
    }
    let [out] = graph.node_outputs_exact::<1>(node)?;
    let ty = match graph.output_kind(out).as_value() {
        Some(t) => t,
        None => return Ok(false),
    };
    let all_ones = match ty.get_unsigned_int(u128::MAX) {
        Some(v) => v,
        None => return Ok(false),
    };
    let [a, b] = graph.node_inputs_exact::<2>(node)?;
    let x_out = match (read_int_const(graph, a), read_int_const(graph, b)) {
        (Some(c), None) if c == all_ones => b,
        (None, Some(c)) if c == all_ones => a,
        _ => return Ok(false),
    };
    let new_out = graph.make_value_node(NodeKind::IntUnaryOp(IntUnaryOp::BitNot), [x_out], ty)?;
    let new_node = graph.get_node_from_output(new_out);
    graph.extend_asm_fingerprint_from(new_node, node);
    let replaced = graph.replace_all_uses(out, new_out)?;
    Ok(replaced)
}

/// If `out` is produced by an `IntConst(v)` node, returns `Some(v)`.
fn read_int_const(graph: &strider_ir::Graph, out: NodeOutputId) -> Option<u128> {
    let node = graph.get_node_from_output(out);
    match graph.node_kind(node) {
        NodeKind::IntConst(v) => Some(*v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! White-box smoke test — full parity test lives in
    //! `crates/strider-analyze/tests/constant_fold_egg_parity.rs`.
    use super::*;
    use crate::opt::test_support::{make_fn, return_kind};
    use strider_ir::IntBinaryOp;

    #[test]
    fn smoke_fold_add() {
        let mut fg = make_fn(|b| {
            let c3 = b.build_int_const(3u64, NodeOutputType::U64).unwrap();
            let c4 = b.build_int_const(4u64, NodeOutputType::U64).unwrap();
            b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .expect("build fixture");
        let res = ConstantFoldEgg::new()
            .optimize_raw(&mut fg.graph, fg.entry)
            .expect("optimize must not error");
        assert!(res.changed(), "expected ConstantFoldEgg to fold const Add");
        assert_eq!(return_kind((&fg).into()).unwrap(), NodeKind::IntConst(7));
    }
}
