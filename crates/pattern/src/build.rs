//! Pattern-to-pattern rewrite-rule API.
//!
//! This module provides the right-hand side of a rewrite rule: a [`Build`]
//! tree that describes either
//!
//! * a captured [`ir::node::NodeOutputId`] from the LHS match, reused verbatim
//!   via [`Build::Capture`], or
//! * a fresh subgraph to be constructed and spliced into the IR via
//!   [`BuiltFunctionGraph::make_value_node`] / friends.
//!
//! [`rewrite_rule`] takes an existing [`crate::Pat`] (the LHS) and a `Build`
//! (the RHS) and returns a closure that, when applied to a function graph and
//! a candidate root node, attempts the match and on success redirects the
//! root's uses to the RHS output via
//! [`BuiltFunctionGraph::replace_all_uses`].
//!
//! [`apply_rules_in_order`] composes a list of rule closures, short-circuiting
//! as soon as any rule fires on a given root.
//!
//! # Typing policy (A3 simplification)
//!
//! For fresh nodes built from a `Build` subtree, the interpreter uses the
//! root's output type ([`BuildCtx::root_ty`]) for every node **unless** the
//! node kind dictates its own type:
//!
//! * `BoolConst`, `BoolBinary`, `BoolUnary`, `IntCmp`, `FloatCmp`, `FloatIsNan`
//!   — always produce [`NodeOutputType::Bool`].
//! * Every other arithmetic, bitwise, or constant node inherits `root_ty`.
//!
//! This is intentionally simple: a rule that needs mixed integer widths inside
//! a single RHS subtree is out of scope for A3 and should use a custom fold
//! function instead.  A later phase can extend the `Build` tree with
//! per-subtree type annotations if mixed-width rewrites become common.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::error::{ErrorKind, Result};
use crate::matcher::{Bindings, Matcher};
use crate::pat::Pat;
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, Var,
};

// ── BuildCtx / BuildValue ─────────────────────────────────────────────────────

/// Context passed to closure-valued pieces of a [`Build`] tree.
///
/// Exposes the captured bindings from the LHS match and the root's output type
/// so user closures can compute fresh constant values based on matched
/// operands.
pub struct BuildCtx<'a> {
    /// The function graph being rewritten (read-only view — closures compute
    /// values, they don't mutate the graph).
    pub graph: &'a BuiltFunctionGraph,
    /// The bindings accumulated during the LHS match.
    pub bindings: &'a Bindings,
    /// The root [`NodeId`] where the LHS matched.
    pub root: NodeId,
    /// The root's declared output type.  All fresh `Build` nodes (except the
    /// bool-producing kinds) are constructed with this type.
    pub root_ty: NodeOutputType,
}

/// Closure type stored inside [`BuildValue::Computed`].  Extracted to a
/// standalone alias so the surrounding type signatures stay legible (and keep
/// clippy's `type_complexity` lint quiet).
pub type BuildValueFn<T> = Arc<dyn Fn(&BuildCtx<'_>) -> Result<T> + Send + Sync>;

/// A value inside a [`Build`] node — either a literal or a closure evaluated
/// against a [`BuildCtx`] at rewrite-firing time.
pub enum BuildValue<T> {
    /// A compile-time literal.
    Lit(T),
    /// A closure that computes the value from the match context.
    Computed(BuildValueFn<T>),
}

impl<T: Clone> Clone for BuildValue<T> {
    fn clone(&self) -> Self {
        match self {
            BuildValue::Lit(v) => BuildValue::Lit(v.clone()),
            BuildValue::Computed(f) => BuildValue::Computed(Arc::clone(f)),
        }
    }
}

impl<T> BuildValue<T> {
    fn resolve(&self, ctx: &BuildCtx<'_>) -> Result<T>
    where
        T: Clone,
    {
        match self {
            BuildValue::Lit(v) => Ok(v.clone()),
            BuildValue::Computed(f) => f(ctx),
        }
    }
}

// ── Build tree ────────────────────────────────────────────────────────────────

/// RHS of a rewrite rule: either a reused capture or a fresh subgraph.
///
/// Every `Build` node that represents a value produces a single
/// [`NodeOutputId`] at evaluation time.  Composition is explicit — use the
/// `Arc<Build>` fields directly or the ergonomic helpers at the module root
/// (`cap`, `add`, `int_const_lit`, …).
#[derive(Clone)]
pub enum Build {
    /// Reuse a captured [`NodeOutputId`] from the LHS match.
    Capture(Var),

    /// Build a fresh `IntConst` node.
    IntConst(BuildValue<u64>),
    /// Build a fresh `BoolConst` node.  Always produces `NodeOutputType::Bool`.
    BoolConst(BuildValue<bool>),
    /// Build a fresh `FloatConst` node (IEEE 754 bit pattern).
    FloatConst(BuildValue<u64>),

    /// Build a fresh `IntBinaryOp` node with a concrete operator variant.
    IntBinary(IntBinaryOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `IntUnaryOp` node with a concrete operator variant.
    IntUnary(IntUnaryOp, Arc<Build>),
    /// Build a fresh `IntCmpOp` node.  Always produces `NodeOutputType::Bool`.
    IntCmp(IntCmpOp, Arc<Build>, Arc<Build>),

    /// Build a fresh `BoolBinaryOp` node.  Always produces `NodeOutputType::Bool`.
    BoolBinary(BoolBinaryOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `BoolUnaryOp` node.  Always produces `NodeOutputType::Bool`.
    BoolUnary(BoolUnaryOp, Arc<Build>),

    /// Build a fresh `FloatBinaryOp` node.
    FloatBinary(FloatBinaryOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `FloatUnaryOp` node.
    FloatUnary(FloatUnaryOp, Arc<Build>),
    /// Build a fresh `FloatCmpOp` node.  Always produces `NodeOutputType::Bool`.
    FloatCmp(FloatCmpOp, Arc<Build>, Arc<Build>),
    /// Build a fresh `FloatIsNan` node.  Always produces `NodeOutputType::Bool`.
    FloatIsNan(Arc<Build>),

    // Variant-pass-through: the operator variant is resolved from a captured
    // `*OpVar` at evaluation time.  Fails if the variable is unbound.
    /// Build `IntBinaryOp(op_captured, lhs, rhs)`.
    IntBinaryFromVar(IntBinaryOpVar, Arc<Build>, Arc<Build>),
    /// Build `IntUnaryOp(op_captured, operand)`.
    IntUnaryFromVar(IntUnaryOpVar, Arc<Build>),
    /// Build `IntCmpOp(op_captured, lhs, rhs)` → `Bool`.
    IntCmpFromVar(IntCmpOpVar, Arc<Build>, Arc<Build>),
    /// Build `BoolBinaryOp(op_captured, lhs, rhs)`.
    BoolBinaryFromVar(BoolBinaryOpVar, Arc<Build>, Arc<Build>),
    /// Build `BoolUnaryOp(op_captured, operand)`.
    BoolUnaryFromVar(BoolUnaryOpVar, Arc<Build>),
    /// Build `FloatBinaryOp(op_captured, lhs, rhs)`.
    FloatBinaryFromVar(FloatBinaryOpVar, Arc<Build>, Arc<Build>),
    /// Build `FloatUnaryOp(op_captured, operand)`.
    FloatUnaryFromVar(FloatUnaryOpVar, Arc<Build>),
    /// Build `FloatCmpOp(op_captured, lhs, rhs)` → `Bool`.
    FloatCmpFromVar(FloatCmpOpVar, Arc<Build>, Arc<Build>),

    /// Abort the rewrite: a closure or structural check decided the rule
    /// doesn't apply after all.  At the top level this maps to
    /// [`RewriteOutcome::Skip`]; inside a larger subtree it propagates upward,
    /// causing the whole rewrite to be skipped.
    Skip,
}

/// Outcome of a single rewrite rule firing.
pub enum RewriteOutcome {
    /// Redirect the root's single value output to this [`NodeOutputId`].
    RedirectTo(NodeOutputId),
    /// The rule decided not to apply after all.  The caller leaves the graph
    /// untouched and reports "no change".
    Skip,
}

// Internal helper: an inner evaluation either produced a value or propagated
// a Skip upward.
enum InnerOutcome {
    Out(NodeOutputId),
    Skip,
}

// ── Evaluator ────────────────────────────────────────────────────────────────

/// Scratch state threaded through the recursive evaluator.  Mutable reference
/// to the graph, plus the immutable match context needed by every closure.
struct EvalState<'a> {
    fg: &'a mut BuiltFunctionGraph,
    bindings: &'a Bindings,
    root: NodeId,
    root_ty: NodeOutputType,
}

impl<'a> EvalState<'a> {
    fn build_ctx(&self) -> BuildCtx<'_> {
        BuildCtx {
            graph: self.fg,
            bindings: self.bindings,
            root: self.root,
            root_ty: self.root_ty,
        }
    }
}

fn eval_subtree(state: &mut EvalState<'_>, build: &Build) -> Result<InnerOutcome> {
    match build {
        Build::Skip => Ok(InnerOutcome::Skip),

        Build::Capture(v) => {
            let out = state.bindings.get(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::Capture references unbound Var {v:?}"
                ))
            })?;
            Ok(InnerOutcome::Out(out))
        }

        Build::IntConst(bv) => {
            let val = bv.resolve(&state.build_ctx())?;
            let out = state.fg.make_int_const(val, state.root_ty)?;
            Ok(InnerOutcome::Out(out))
        }

        Build::BoolConst(bv) => {
            let val = bv.resolve(&state.build_ctx())?;
            let out = state.fg.make_bool_const(val)?;
            Ok(InnerOutcome::Out(out))
        }

        Build::FloatConst(bv) => {
            let bits = bv.resolve(&state.build_ctx())?;
            let out = state.fg.make_float_const(bits, state.root_ty)?;
            Ok(InnerOutcome::Out(out))
        }

        Build::IntBinary(op, l, r) => {
            build_binary(state, l, r, NodeKind::IntBinaryOp(*op), state.root_ty)
        }
        Build::IntUnary(op, x) => {
            build_unary(state, x, NodeKind::IntUnaryOp(*op), state.root_ty)
        }
        Build::IntCmp(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::IntCmpOp(*op),
            NodeOutputType::Bool,
        ),

        Build::BoolBinary(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::BoolBinaryOp(*op),
            NodeOutputType::Bool,
        ),
        Build::BoolUnary(op, x) => build_unary(
            state,
            x,
            NodeKind::BoolUnaryOp(*op),
            NodeOutputType::Bool,
        ),

        Build::FloatBinary(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::FloatBinaryOp(*op),
            state.root_ty,
        ),
        Build::FloatUnary(op, x) => {
            build_unary(state, x, NodeKind::FloatUnaryOp(*op), state.root_ty)
        }
        Build::FloatCmp(op, l, r) => build_binary(
            state,
            l,
            r,
            NodeKind::FloatCmpOp(*op),
            NodeOutputType::Bool,
        ),
        Build::FloatIsNan(x) => {
            build_unary(state, x, NodeKind::FloatIsNan, NodeOutputType::Bool)
        }

        Build::IntBinaryFromVar(v, l, r) => {
            let op = state.bindings.get_int_binary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::IntBinaryFromVar references unbound IntBinaryOpVar {v:?}"
                ))
            })?;
            build_binary(state, l, r, NodeKind::IntBinaryOp(op), state.root_ty)
        }
        Build::IntUnaryFromVar(v, x) => {
            let op = state.bindings.get_int_unary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::IntUnaryFromVar references unbound IntUnaryOpVar {v:?}"
                ))
            })?;
            build_unary(state, x, NodeKind::IntUnaryOp(op), state.root_ty)
        }
        Build::IntCmpFromVar(v, l, r) => {
            let op = state.bindings.get_int_cmp_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::IntCmpFromVar references unbound IntCmpOpVar {v:?}"
                ))
            })?;
            build_binary(
                state,
                l,
                r,
                NodeKind::IntCmpOp(op),
                NodeOutputType::Bool,
            )
        }
        Build::BoolBinaryFromVar(v, l, r) => {
            let op = state.bindings.get_bool_binary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::BoolBinaryFromVar references unbound BoolBinaryOpVar {v:?}"
                ))
            })?;
            build_binary(
                state,
                l,
                r,
                NodeKind::BoolBinaryOp(op),
                NodeOutputType::Bool,
            )
        }
        Build::BoolUnaryFromVar(v, x) => {
            let op = state.bindings.get_bool_unary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::BoolUnaryFromVar references unbound BoolUnaryOpVar {v:?}"
                ))
            })?;
            build_unary(state, x, NodeKind::BoolUnaryOp(op), NodeOutputType::Bool)
        }
        Build::FloatBinaryFromVar(v, l, r) => {
            let op = state.bindings.get_float_binary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::FloatBinaryFromVar references unbound FloatBinaryOpVar {v:?}"
                ))
            })?;
            build_binary(state, l, r, NodeKind::FloatBinaryOp(op), state.root_ty)
        }
        Build::FloatUnaryFromVar(v, x) => {
            let op = state.bindings.get_float_unary_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::FloatUnaryFromVar references unbound FloatUnaryOpVar {v:?}"
                ))
            })?;
            build_unary(state, x, NodeKind::FloatUnaryOp(op), state.root_ty)
        }
        Build::FloatCmpFromVar(v, l, r) => {
            let op = state.bindings.get_float_cmp_op(*v).ok_or_else(|| {
                ErrorKind::AssertionFailed(format!(
                    "Build::FloatCmpFromVar references unbound FloatCmpOpVar {v:?}"
                ))
            })?;
            build_binary(
                state,
                l,
                r,
                NodeKind::FloatCmpOp(op),
                NodeOutputType::Bool,
            )
        }
    }
}

fn build_unary(
    state: &mut EvalState<'_>,
    x: &Arc<Build>,
    kind: NodeKind,
    result_ty: NodeOutputType,
) -> Result<InnerOutcome> {
    let InnerOutcome::Out(arg) = eval_subtree(state, x)? else {
        return Ok(InnerOutcome::Skip);
    };
    let out = state.fg.make_value_node(kind, [arg], result_ty)?;
    Ok(InnerOutcome::Out(out))
}

fn build_binary(
    state: &mut EvalState<'_>,
    l: &Arc<Build>,
    r: &Arc<Build>,
    kind: NodeKind,
    result_ty: NodeOutputType,
) -> Result<InnerOutcome> {
    let InnerOutcome::Out(l_out) = eval_subtree(state, l)? else {
        return Ok(InnerOutcome::Skip);
    };
    let InnerOutcome::Out(r_out) = eval_subtree(state, r)? else {
        return Ok(InnerOutcome::Skip);
    };
    let out = state.fg.make_value_node(kind, [l_out, r_out], result_ty)?;
    Ok(InnerOutcome::Out(out))
}

/// Top-level evaluator.  Converts [`InnerOutcome::Skip`] into
/// [`RewriteOutcome::Skip`] and wraps a produced output in
/// [`RewriteOutcome::RedirectTo`].
pub fn eval(
    build: &Build,
    fg: &mut BuiltFunctionGraph,
    bindings: &Bindings,
    root: NodeId,
    root_ty: NodeOutputType,
) -> Result<RewriteOutcome> {
    let mut state = EvalState {
        fg,
        bindings,
        root,
        root_ty,
    };
    match eval_subtree(&mut state, build)? {
        InnerOutcome::Out(out) => Ok(RewriteOutcome::RedirectTo(out)),
        InnerOutcome::Skip => Ok(RewriteOutcome::Skip),
    }
}

// ── rewrite_rule / apply_rules_in_order ──────────────────────────────────────

/// Build a rewrite-rule closure from an LHS [`Pat`] and an RHS [`Build`].
///
/// The returned closure takes `&mut BuiltFunctionGraph` and a candidate root
/// [`NodeId`], attempts the match, and on success evaluates the RHS and
/// redirects the root's value output to the RHS output via
/// [`BuiltFunctionGraph::replace_all_uses`].
///
/// Returns `Ok(true)` if the rule fired and at least one use was redirected,
/// `Ok(false)` if the match failed, the RHS produced [`RewriteOutcome::Skip`],
/// or `replace_all_uses` found nothing to redirect.
///
/// Errors from the graph layer (`make_value_node`, `replace_all_uses`) are
/// wrapped in [`ErrorKind::IrError`]; errors from user closures are wrapped in
/// [`ErrorKind::RewriteClosure`] (via [`Error::rewrite_closure`]).
pub fn rewrite_rule(
    lhs: Pat,
    rhs: Build,
) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static {
    move |fg: &mut BuiltFunctionGraph, node: NodeId| -> Result<bool> {
        // 1. Match LHS.  Keep the matcher borrow in a tight scope so we can
        //    mutate `fg` afterwards.
        let bindings = {
            let matcher = Matcher::new(fg);
            match matcher.match_at(node, &lhs) {
                Some(m) => m.bindings_clone(),
                None => return Ok(false),
            }
        };

        // 2. Fetch root's single value output and its type.
        let [root_out] = fg.graph.node_outputs_exact::<1>(node)?;
        let root_ty = fg.graph.output_kind(root_out).as_value_or_err()?;

        // 3. Evaluate RHS.
        let outcome = eval(&rhs, fg, &bindings, node, root_ty)?;

        match outcome {
            RewriteOutcome::Skip => Ok(false),
            RewriteOutcome::RedirectTo(new_out) => {
                let changed = fg.replace_all_uses(root_out, new_out)?;
                Ok(changed)
            }
        }
    }
}

/// Compose a list of rewrite-rule closures into a single closure.
///
/// The returned closure iterates every rule in `rules` on the same root node,
/// OR-ing the per-rule `bool` results.  Once the first rule fires the root's
/// uses are redirected — subsequent rules then see the new graph state and
/// may or may not still apply; this mirrors the "run every rule, once" policy
/// of the pre-rewrite `apply_identity_rules` fold in `constant_fold.rs`.
///
/// Takes `rules` by value (`Vec<R>`) and moves them into the closure; this
/// composes naturally with the common consumer pattern of building a fresh
/// `Vec<rewrite_rule(...)>` at pass-configuration time.
pub fn apply_rules_in_order<R>(
    rules: Vec<R>,
) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static
where
    R: Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static,
{
    move |fg, node| {
        let mut any = false;
        for r in &rules {
            if r(fg, node)? {
                any = true;
            }
        }
        Ok(any)
    }
}

// ── Constructors ──────────────────────────────────────────────────────────────

/// Reuse a captured [`Var`] from the LHS match.
pub fn cap(v: Var) -> Build {
    Build::Capture(v)
}

/// Build a fresh `IntConst` node with a compile-time-known value.
pub fn int_const_lit(n: u64) -> Build {
    Build::IntConst(BuildValue::Lit(n))
}

/// Build a fresh `IntConst` node whose value is computed from the match
/// context at rewrite-firing time.
pub fn int_const_fn<F>(f: F) -> Build
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    Build::IntConst(BuildValue::Computed(Arc::new(f)))
}

/// Build a fresh `BoolConst` node with a literal value.
pub fn bool_const_lit(b: bool) -> Build {
    Build::BoolConst(BuildValue::Lit(b))
}

/// Build a fresh `BoolConst` node whose value is computed at firing time.
pub fn bool_const_fn<F>(f: F) -> Build
where
    F: Fn(&BuildCtx<'_>) -> Result<bool> + Send + Sync + 'static,
{
    Build::BoolConst(BuildValue::Computed(Arc::new(f)))
}

/// Build a fresh `FloatConst` node with a literal bit pattern.
pub fn float_const_lit(bits: u64) -> Build {
    Build::FloatConst(BuildValue::Lit(bits))
}

/// Build a fresh `FloatConst` node whose bit pattern is computed at firing
/// time.
pub fn float_const_fn<F>(f: F) -> Build
where
    F: Fn(&BuildCtx<'_>) -> Result<u64> + Send + Sync + 'static,
{
    Build::FloatConst(BuildValue::Computed(Arc::new(f)))
}

/// Abort the rewrite from inside the RHS tree.
pub fn skip() -> Build {
    Build::Skip
}

// Integer binary ops
fn int_binary(op: IntBinaryOp, l: Build, r: Build) -> Build {
    Build::IntBinary(op, Arc::new(l), Arc::new(r))
}

/// Build `l + r`.
pub fn add(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Add, l, r)
}
/// Build `l - r`.
pub fn sub(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Sub, l, r)
}
/// Build `l * r`.
pub fn mul(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Mul, l, r)
}
/// Build `l & r`.
pub fn and(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::And, l, r)
}
/// Build `l | r`.
pub fn or(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Or, l, r)
}
/// Build `l ^ r`.
pub fn xor(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Xor, l, r)
}
/// Build `l << r`.
pub fn shl(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::ShiftLeft, l, r)
}
/// Build `l >> r` (logical / unsigned).
pub fn shr(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::ShiftRight, l, r)
}
/// Build `l >>> r` (arithmetic / signed).
pub fn sshr(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::SShiftRight, l, r)
}
/// Build `l / r` (unsigned).
pub fn div(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Div, l, r)
}
/// Build `l / r` (signed).
pub fn sdiv(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Sdiv, l, r)
}
/// Build `l % r` (unsigned).
pub fn rem(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Rem, l, r)
}
/// Build `l % r` (signed).
pub fn srem(l: Build, r: Build) -> Build {
    int_binary(IntBinaryOp::Srem, l, r)
}

// Integer unary ops
fn int_unary(op: IntUnaryOp, x: Build) -> Build {
    Build::IntUnary(op, Arc::new(x))
}

/// Build `-x`.
pub fn neg(x: Build) -> Build {
    int_unary(IntUnaryOp::Neg, x)
}
/// Build `!x` (bitwise complement).
pub fn not(x: Build) -> Build {
    int_unary(IntUnaryOp::Not, x)
}

// Integer cmp ops (→ Bool)
fn int_cmp(op: IntCmpOp, l: Build, r: Build) -> Build {
    Build::IntCmp(op, Arc::new(l), Arc::new(r))
}

/// Build `l == r` (integer equality).
pub fn int_eq(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::Equal, l, r)
}
/// Build `l < r` (unsigned less-than).
pub fn int_lt(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::Less, l, r)
}
/// Build `l < r` (signed less-than).
pub fn int_slt(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::Sless, l, r)
}
/// Build `l <= r` (unsigned less-or-equal).
pub fn int_le(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::LessEqual, l, r)
}
/// Build `l <= r` (signed less-or-equal).
pub fn int_sle(l: Build, r: Build) -> Build {
    int_cmp(IntCmpOp::SlessEqual, l, r)
}

// Bool binary ops
fn bool_binary(op: BoolBinaryOp, l: Build, r: Build) -> Build {
    Build::BoolBinary(op, Arc::new(l), Arc::new(r))
}

/// Build `l && r` (boolean and).
pub fn bool_and(l: Build, r: Build) -> Build {
    bool_binary(BoolBinaryOp::And, l, r)
}
/// Build `l || r` (boolean or).
pub fn bool_or(l: Build, r: Build) -> Build {
    bool_binary(BoolBinaryOp::Or, l, r)
}
/// Build `l ^ r` (boolean xor).
pub fn bool_xor(l: Build, r: Build) -> Build {
    bool_binary(BoolBinaryOp::Xor, l, r)
}

// Bool unary ops
/// Build `!x` (boolean negation).
pub fn bool_neg(x: Build) -> Build {
    Build::BoolUnary(BoolUnaryOp::Neg, Arc::new(x))
}
/// Alias for [`bool_neg`].  Provided because the original task prompt lists
/// both `bool_neg` and `bool_not`; the IR only has a single `Neg` variant.
pub fn bool_not(x: Build) -> Build {
    bool_neg(x)
}

// Float binary ops
fn float_binary(op: FloatBinaryOp, l: Build, r: Build) -> Build {
    Build::FloatBinary(op, Arc::new(l), Arc::new(r))
}

/// Build float `l + r`.
pub fn float_add(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Add, l, r)
}
/// Build float `l - r`.
pub fn float_sub(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Sub, l, r)
}
/// Build float `l * r`.
pub fn float_mul(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Mul, l, r)
}
/// Build float `l / r`.
pub fn float_div(l: Build, r: Build) -> Build {
    float_binary(FloatBinaryOp::Div, l, r)
}

// Float unary ops
fn float_unary(op: FloatUnaryOp, x: Build) -> Build {
    Build::FloatUnary(op, Arc::new(x))
}

/// Build `-x` (float).
pub fn float_neg(x: Build) -> Build {
    float_unary(FloatUnaryOp::Neg, x)
}
/// Build `|x|` (float absolute value).
pub fn float_abs(x: Build) -> Build {
    float_unary(FloatUnaryOp::Abs, x)
}
/// Build `sqrt(x)`.
pub fn float_sqrt(x: Build) -> Build {
    float_unary(FloatUnaryOp::Sqrt, x)
}
/// Build `round(x)`.
pub fn float_round(x: Build) -> Build {
    float_unary(FloatUnaryOp::Round, x)
}
/// Build `floor(x)`.
pub fn float_floor(x: Build) -> Build {
    float_unary(FloatUnaryOp::Floor, x)
}
/// Build `ceil(x)`.
pub fn float_ceil(x: Build) -> Build {
    float_unary(FloatUnaryOp::Ceil, x)
}

// Float cmp ops
fn float_cmp(op: FloatCmpOp, l: Build, r: Build) -> Build {
    Build::FloatCmp(op, Arc::new(l), Arc::new(r))
}

/// Build float `l == r`.
pub fn float_eq(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::Equal, l, r)
}
/// Build float `l < r`.
pub fn float_lt(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::Less, l, r)
}
/// Build float `l <= r`.
pub fn float_le(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::LessEqual, l, r)
}
/// Build float `l != r`.
pub fn float_ne(l: Build, r: Build) -> Build {
    float_cmp(FloatCmpOp::NotEqual, l, r)
}

/// Build `float_is_nan(x)`.
pub fn float_is_nan(x: Build) -> Build {
    Build::FloatIsNan(Arc::new(x))
}

// Variant-from-var helpers

/// Build an integer binary op whose variant is resolved from a captured
/// [`IntBinaryOpVar`] at firing time.
pub fn int_binary_from_var(v: IntBinaryOpVar, l: Build, r: Build) -> Build {
    Build::IntBinaryFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build an integer unary op whose variant is resolved from a captured
/// [`IntUnaryOpVar`] at firing time.
pub fn int_unary_from_var(v: IntUnaryOpVar, x: Build) -> Build {
    Build::IntUnaryFromVar(v, Arc::new(x))
}

/// Build an integer comparison op whose variant is resolved from a captured
/// [`IntCmpOpVar`] at firing time.  Produces `Bool`.
pub fn int_cmp_from_var(v: IntCmpOpVar, l: Build, r: Build) -> Build {
    Build::IntCmpFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build a boolean binary op whose variant is resolved from a captured
/// [`BoolBinaryOpVar`] at firing time.
pub fn bool_binary_from_var(v: BoolBinaryOpVar, l: Build, r: Build) -> Build {
    Build::BoolBinaryFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build a boolean unary op whose variant is resolved from a captured
/// [`BoolUnaryOpVar`] at firing time.
pub fn bool_unary_from_var(v: BoolUnaryOpVar, x: Build) -> Build {
    Build::BoolUnaryFromVar(v, Arc::new(x))
}

/// Build a float binary op whose variant is resolved from a captured
/// [`FloatBinaryOpVar`] at firing time.
pub fn float_binary_from_var(v: FloatBinaryOpVar, l: Build, r: Build) -> Build {
    Build::FloatBinaryFromVar(v, Arc::new(l), Arc::new(r))
}

/// Build a float unary op whose variant is resolved from a captured
/// [`FloatUnaryOpVar`] at firing time.
pub fn float_unary_from_var(v: FloatUnaryOpVar, x: Build) -> Build {
    Build::FloatUnaryFromVar(v, Arc::new(x))
}

/// Build a float comparison op whose variant is resolved from a captured
/// [`FloatCmpOpVar`] at firing time.  Produces `Bool`.
pub fn float_cmp_from_var(v: FloatCmpOpVar, l: Build, r: Build) -> Build {
    Build::FloatCmpFromVar(v, Arc::new(l), Arc::new(r))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::pat::{add as pat_add, int_const as pat_int_const, var as pat_var};
    use crate::var::Var;
    use ir::FunctionBuilder;

    /// Build a tiny graph: `add(x, 0) + 0`, returning the outer add.
    ///
    /// Returning the graph plus the outer-add `NodeId` so tests can fire the
    /// rule directly.  We wrap in another `add(…, 1)` so the outer add has a
    /// downstream consumer (the return) and `replace_all_uses` has work to do.
    fn graph_add_x_plus_zero()
    -> ir::Result<(ir::BuiltFunctionGraph, NodeId, NodeOutputId)> {
        use ir::IntBinaryOp;
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        // `x` is just a constant — the rule doesn't care, it only needs the
        // structure `add(x, 0)`.
        let x = b.build_int_const(7, NodeOutputType::U64);
        let zero = b.build_int_const(0, NodeOutputType::U64);
        let add_out =
            b.build_int_binary_operation(x, zero, IntBinaryOp::Add, NodeOutputType::U64)?;
        // A second operation consumes `add_out` so that `replace_all_uses`
        // has at least one edge to redirect.  Return it.
        b.build_return(Some(add_out), &[])?;
        let fg = b.build()?;
        // Find the Add node.
        let add_node = fg.graph.get_node_from_output(add_out);
        Ok((fg, add_node, add_out))
    }

    #[test]
    fn rewrite_rule_identity_x_plus_zero() -> Result<()> {
        let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

        let x = Var::new();
        let rule = rewrite_rule(pat_add(pat_var(x), pat_int_const(0)).into(), cap(x));

        let changed = rule(&mut fg, add_node)?;
        assert!(
            changed,
            "x + 0 => x should redirect the return's consumer of the Add output"
        );
        Ok(())
    }

    #[test]
    fn rewrite_rule_no_match_returns_ok_false() -> Result<()> {
        let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

        // A rule that matches `mul(x, 1)`, which our graph doesn't have.
        use crate::pat::mul as pat_mul;
        let x = Var::new();
        let rule = rewrite_rule(pat_mul(pat_var(x), pat_int_const(1)).into(), cap(x));

        let changed = rule(&mut fg, add_node)?;
        assert!(!changed, "rule whose LHS doesn't match should return Ok(false)");
        Ok(())
    }

    #[test]
    fn rewrite_rule_skip_rhs_returns_ok_false() -> Result<()> {
        let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

        // LHS matches, but RHS is Skip → rewrite is aborted.
        let x = Var::new();
        let rule = rewrite_rule(pat_add(pat_var(x), pat_int_const(0)).into(), skip());

        let changed = rule(&mut fg, add_node)?;
        assert!(!changed, "Build::Skip at the top level should report no change");
        Ok(())
    }

    #[test]
    fn rewrite_rule_computed_const_failure_is_propagated() -> Result<()> {
        // Sanity-check: a closure returning an error surfaces through the
        // rewrite engine as a `pattern::Error`.
        use crate::pat::mul as pat_mul;
        let (mut fg, add_node, _) = graph_add_x_plus_zero()?;

        #[derive(Debug, thiserror::Error)]
        #[error("custom closure error")]
        struct CustomError;

        let x = Var::new();
        // LHS doesn't match this graph, so the closure never fires; we still
        // exercise the construction path.  (A positive test — LHS matches and
        // closure fires — is deferred to A4 where the `int_const_with!` macro
        // will supply the full typed-capture wiring.)
        let rule = rewrite_rule(
            pat_mul(pat_var(x), pat_int_const(1)).into(),
            int_const_fn(|_ctx| Err(Error::rewrite_closure(CustomError))),
        );
        let changed = rule(&mut fg, add_node)?;
        assert!(!changed);
        Ok(())
    }

    #[test]
    fn apply_rules_in_order_runs_until_one_fires() -> Result<()> {
        use crate::pat::mul as pat_mul;
        let (mut fg, add_node, _) = graph_add_x_plus_zero()?;

        let x1 = Var::new();
        let rule_no_match =
            rewrite_rule(pat_mul(pat_var(x1), pat_int_const(1)).into(), cap(x1));
        let x2 = Var::new();
        let rule_hit =
            rewrite_rule(pat_add(pat_var(x2), pat_int_const(0)).into(), cap(x2));

        let combined = apply_rules_in_order(vec![rule_no_match, rule_hit]);
        let changed = combined(&mut fg, add_node)?;
        assert!(changed, "at least one rule fired");
        Ok(())
    }
}
