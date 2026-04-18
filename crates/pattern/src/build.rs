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
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::error::{ErrorKind, Result};
use crate::matcher::{Bindings, Matcher};
use crate::pat::Pat;
use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
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
    lhs: impl Into<Pat>,
    rhs: Build,
) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static {
    let lhs: Pat = lhs.into();
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

        // 3. Evaluate RHS.  A closure inside the tree may opt out of the
        //    rewrite by returning `Err(pattern::Error::skip())`; catch that
        //    sentinel here and convert it to "no change" instead of a hard
        //    error.  All other errors propagate.
        let outcome = match eval(&rhs, fg, &bindings, node, root_ty) {
            Ok(o) => o,
            Err(e) if e.is_skip() => return Ok(false),
            Err(e) => return Err(e),
        };

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

/// Type-erased rewrite-rule closure.
///
/// Each call to [`rewrite_rule`] returns a distinct opaque `impl Fn` type, so a
/// `Vec<impl Fn>` can only hold rules with identical signatures — in practice,
/// only a single rule.  Consumers composing a list of heterogeneous rules need
/// to box each one to a common trait-object type; this alias plus
/// [`boxed_rule`] factor that boilerplate out of every call site.
pub type BoxedRule =
    Box<dyn Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync>;

/// Wraps a rewrite-rule closure in a [`BoxedRule`] for storage in a
/// `Vec<BoxedRule>` alongside rules built from other LHS/RHS shapes.
///
/// Typical use:
///
/// ```rust,ignore
/// let rules: Vec<BoxedRule> = vec![
///     boxed_rule(rewrite_rule(add(var(x), int_const(0)), build::cap(x))),
///     boxed_rule(rewrite_rule(sub(var(x), var(x)),       build::int_const_lit(0))),
///     // …
/// ];
/// let apply = apply_rules_in_order(rules);
/// ```
pub fn boxed_rule<R>(r: R) -> BoxedRule
where
    R: Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool> + Send + Sync + 'static,
{
    Box::new(r)
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

// ── FromCtx trait ─────────────────────────────────────────────────────────────

/// Extracts a typed value from a [`BuildCtx`] given a capture variable.
///
/// Used by the [`int_const_with!`], [`bool_const_with!`], and
/// [`float_const_with!`] macros to turn a capture identifier into its
/// concrete value without per-closure boilerplate.
///
/// Every capture type added in Phases A1/A2 has an impl: [`Var`], [`NodeVar`],
/// [`IntVar`], [`BoolVar`], [`FloatVar`], and the eight `*OpVar` types.
///
/// # Errors
///
/// Returns [`ErrorKind::MissingBinding`] if the capture was not bound during
/// the LHS match — this indicates a pattern-authoring bug (the capture
/// appears in the RHS but not in the LHS, or the LHS matches a node that
/// doesn't populate that binding).
pub trait FromCtx {
    /// The Rust-native type extracted from the context.
    type Output;
    /// Retrieve the value bound to `self` inside `ctx`.
    ///
    /// Despite the `from_` prefix, this takes `&self` — the capture
    /// variable *is* the key used to look up the binding.  The trait is
    /// named from the *caller's* perspective: "derive a value from the
    /// [`BuildCtx`]".
    #[allow(clippy::wrong_self_convention)]
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output>;
}

impl FromCtx for Var {
    type Output = NodeOutputId;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("Var").into())
    }
}

impl FromCtx for NodeVar {
    type Output = NodeId;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_node(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("NodeVar").into())
    }
}

impl FromCtx for IntVar {
    type Output = u64;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntVar").into())
    }
}

impl FromCtx for BoolVar {
    type Output = bool;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_bool(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("BoolVar").into())
    }
}

impl FromCtx for FloatVar {
    type Output = u64;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_bits(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatVar").into())
    }
}

impl FromCtx for IntBinaryOpVar {
    type Output = IntBinaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int_binary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntBinaryOpVar").into())
    }
}

impl FromCtx for IntUnaryOpVar {
    type Output = IntUnaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int_unary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntUnaryOpVar").into())
    }
}

impl FromCtx for IntCmpOpVar {
    type Output = IntCmpOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_int_cmp_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("IntCmpOpVar").into())
    }
}

impl FromCtx for BoolBinaryOpVar {
    type Output = BoolBinaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_bool_binary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("BoolBinaryOpVar").into())
    }
}

impl FromCtx for BoolUnaryOpVar {
    type Output = BoolUnaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_bool_unary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("BoolUnaryOpVar").into())
    }
}

impl FromCtx for FloatBinaryOpVar {
    type Output = FloatBinaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_binary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatBinaryOpVar").into())
    }
}

impl FromCtx for FloatUnaryOpVar {
    type Output = FloatUnaryOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_unary_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatUnaryOpVar").into())
    }
}

impl FromCtx for FloatCmpOpVar {
    type Output = FloatCmpOp;
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output> {
        ctx.bindings
            .get_float_cmp_op(*self)
            .ok_or_else(|| ErrorKind::MissingBinding("FloatCmpOpVar").into())
    }
}

// ── first_value_input_type helper ─────────────────────────────────────────────

/// Returns the [`NodeOutputType`] of the root node's **first** value input
/// (input index 0), if that input is a value-typed output.
///
/// Used by the `int_const_with!` / `bool_const_with!` / `float_const_with!`
/// macros to auto-bind an `in_ty` identifier so rewrite-rule closures can
/// refer to the producing-type of the root's first operand — useful for
/// unary ops like `Popcount`, `Lzcount`, `Truncate`, `SignExtend`, and also
/// for `IntCmpOp(lhs, rhs)` where the comparison's *input* type (needed by
/// `eval_int_cmp` for signed/carry/borrow handling) differs from the
/// root's *output* type (always `Bool`).
///
/// Returns `None` if:
/// * the root has zero inputs, or
/// * the first input isn't a value edge (e.g. a control-typed input on a
///   hypothetical control-first op — the generic helper shouldn't assume).
///
/// Rule bodies that need the type should propagate a missing-type failure
/// with `in_ty.ok_or(...)?`.
pub fn first_value_input_type(ctx: &BuildCtx<'_>) -> Option<NodeOutputType> {
    let inputs = ctx.graph.graph.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.graph.graph.output_kind(inp) {
        NodeOutputKind::OutputType(t) => Some(t),
        _ => None,
    }
}

// ── Const-computed macros ─────────────────────────────────────────────────────

/// Builds an `IntConst` node whose value is computed from LHS captures.
///
/// # Syntax
///
/// ```text
/// int_const_with!([cap1, cap2, ...] => expression)
/// ```
///
/// Each `capN` is a capture identifier that also appears in the LHS pattern.
/// The macro expands to an [`int_const_fn`] closure that binds each capture
/// to its concrete value via [`FromCtx`] and evaluates the body, wrapping
/// the result in `Ok`.
///
/// Two special identifiers, if present in the bracket list, are bound to
/// graph-derived values rather than looked up via [`FromCtx`]:
///
/// * `ty` — the root node's output type ([`BuildCtx::root_ty`]).
/// * `in_ty` — `Option<NodeOutputType>`: the type of the root's single
///   value input, when the root has exactly one input.  Use
///   `in_ty.ok_or(...)?` if the rule truly requires it.
///
/// **Do not use `ty` or `in_ty` as your own capture names** — they are
/// reserved by the macro.
///
/// # Example
///
/// ```rust,ignore
/// use pattern::{rewrite_rule, IntVar, any_int_const, and, var, Var};
/// use pattern::build;
/// use pattern::int_const_with;
///
/// let c1 = IntVar::new();
/// let c2 = IntVar::new();
/// let x  = Var::new();
///
/// let rule = rewrite_rule(
///     and(and(var(x), any_int_const(c1)), any_int_const(c2)),
///     build::and(
///         build::cap(x),
///         int_const_with!([c1, c2] => c1 & c2),
///     ),
/// );
/// ```
#[macro_export]
macro_rules! int_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::build::int_const_fn(move |__strider_ctx: &$crate::build::BuildCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Builds a `BoolConst` node whose value is computed from LHS captures.
///
/// Same grammar as [`int_const_with!`] but returns a `bool` from the body.
#[macro_export]
macro_rules! bool_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::build::bool_const_fn(move |__strider_ctx: &$crate::build::BuildCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed from
/// LHS captures.
///
/// Same grammar as [`int_const_with!`]; the body must evaluate to `u64`
/// (the bit pattern).
#[macro_export]
macro_rules! float_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::build::float_const_fn(move |__strider_ctx: &$crate::build::BuildCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Internal helper: a tt-muncher that recursively expands a capture list
/// into `let` bindings.
///
/// Invoked by [`int_const_with!`], [`bool_const_with!`], and
/// [`float_const_with!`].  Each capture identifier in the list becomes a
/// `let <ident> = …;` in the closure body.
///
/// Two identifiers are special-cased and bound to graph-derived values:
///
/// * `ty` — expands to `let ty = __ctx.root_ty;`
/// * `in_ty` — expands to
///   `let in_ty = $crate::build::first_value_input_type(__ctx);`
///
/// All other identifiers are treated as capture variables and resolved via
/// [`FromCtx::from_ctx`], which returns `Result<_>`; the surrounding closure
/// body uses `?` to propagate a missing binding as an error.
///
/// Being a tt-muncher (rather than a `$(...)*` expansion) is what lets the
/// macro name its own identifiers like `ty` and `in_ty` inside `$body`
/// despite macro hygiene — each special rule matches the *literal* ident
/// `ty` / `in_ty` and the resulting `let` binding lives in the same
/// expansion context as the user's body.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bindings {
    // Terminal: nothing left to consume.
    ($ctx:ident;) => {};

    // General case: capture the first identifier into `$cap:ident` so the
    // resulting `let $cap = …;` carries the caller's hygiene context, then
    // dispatch on whether it spelled `ty` / `in_ty` via a helper macro.
    // We pass `$cap` twice: once as the literal-match selector, once as
    // the hygiene-bearing metavariable the inner macro will re-emit.
    ($ctx:ident; $cap:ident $(,)?) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
    };
    ($ctx:ident; $cap:ident, $($rest:tt)*) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
        $crate::__const_with_bindings!($ctx; $($rest)*);
    };
}

/// Emits a single `let`-binding inside the closure body.  Dispatches on
/// the *spelling* of the first ident — `ty` / `in_ty` bind graph-derived
/// values; anything else falls back to `FromCtx::from_ctx`.
///
/// The caller passes the ident **twice**: the first position is a
/// literal-match selector (matches by spelling only), and the second is
/// captured as `$hy:ident` and carries the caller's hygiene context, so
/// `let $hy = …;` introduces a binding visible in the user's `$body`.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bind_one {
    ($ctx:ident, ty, $hy:ident) => {
        let $hy = $ctx.root_ty;
        let _ = &$hy;
    };
    ($ctx:ident, in_ty, $hy:ident) => {
        let $hy = $crate::build::first_value_input_type($ctx);
        let _ = &$hy;
    };
    ($ctx:ident, $_sel:ident, $hy:ident) => {
        let $hy = $crate::build::FromCtx::from_ctx(&$hy, $ctx)?;
    };
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
        let rule = rewrite_rule(pat_add(pat_var(x), pat_int_const(0)), cap(x));

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
        let rule = rewrite_rule(pat_mul(pat_var(x), pat_int_const(1)), cap(x));

        let changed = rule(&mut fg, add_node)?;
        assert!(!changed, "rule whose LHS doesn't match should return Ok(false)");
        Ok(())
    }

    #[test]
    fn rewrite_rule_skip_rhs_returns_ok_false() -> Result<()> {
        let (mut fg, add_node, _add_out) = graph_add_x_plus_zero()?;

        // LHS matches, but RHS is Skip → rewrite is aborted.
        let x = Var::new();
        let rule = rewrite_rule(pat_add(pat_var(x), pat_int_const(0)), skip());

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
            pat_mul(pat_var(x), pat_int_const(1)),
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
            rewrite_rule(pat_mul(pat_var(x1), pat_int_const(1)), cap(x1));
        let x2 = Var::new();
        let rule_hit =
            rewrite_rule(pat_add(pat_var(x2), pat_int_const(0)), cap(x2));

        let combined = apply_rules_in_order(vec![rule_no_match, rule_hit]);
        let changed = combined(&mut fg, add_node)?;
        assert!(changed, "at least one rule fired");
        Ok(())
    }

    // ── int_const_with! / bool_const_with! / float_const_with! ────────────────

    // `int_const_with!`, `bool_const_with!`, and `float_const_with!` are
    // `#[macro_export]` macros and are addressed via `$crate::` inside the
    // pattern crate — no extra `use` is required here.

    /// Macro expansion with zero captures still compiles and produces a
    /// valid `Build::IntConst`.  Fires the rule against `add(x, 0)` so we
    /// can smoke-test end-to-end without relying on any other capture
    /// wiring.
    #[test]
    fn int_const_with_zero_captures_compiles_and_runs() -> Result<()> {
        let (mut fg, add_node, _) = graph_add_x_plus_zero()?;

        let x = Var::new();
        let rule = rewrite_rule(
            pat_add(pat_var(x), pat_int_const(0)),
            int_const_with!([] => 42u64),
        );
        let changed = rule(&mut fg, add_node)?;
        assert!(changed, "rule should fire and redirect uses");
        Ok(())
    }

    /// Single-capture `int_const_with!` body referencing the captured
    /// [`IntVar`] and the auto-bound `in_ty`.  Matches
    /// `Popcount(IntConst(0b1011, U32))`, rewrites to `IntConst(3, U32)`.
    #[test]
    fn int_const_with_popcount_rewrite() -> Result<()> {
        use crate::pat::{any_int_const, popcount};
        use crate::var::IntVar;

        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let c = b.build_int_const(0b1011, NodeOutputType::U32);
        let pc_out = b.build_popcount(c, NodeOutputType::U32)?;
        b.build_return(Some(pc_out), &[])?;
        let mut fg = b.build()?;

        let pc_node = fg.graph.get_node_from_output(pc_out);

        let v = IntVar::new();
        let rule = rewrite_rule(
            popcount(any_int_const(v)),
            int_const_with!([v, in_ty] => {
                // `in_ty` is bound by the macro to `Option<NodeOutputType>`;
                // narrow it and unwrap for this test.
                let ty_in: NodeOutputType = in_ty.ok_or_else(|| {
                    Error::rewrite_closure(std::io::Error::other(
                        "expected integer input type",
                    ))
                })?;
                ty_in.get_unsigned_int(v).unwrap_or(0).count_ones() as u64
            }),
        );
        let changed = rule(&mut fg, pc_node)?;
        assert!(changed, "popcount rule should fire");

        // The Return's retval should now be the new IntConst(3, U32).
        // Locate it via the return node.
        let ret_node = {
            let mut found = None;
            for n in fg.preorder() {
                if matches!(fg.graph.node_kind(n), NodeKind::Return) {
                    found = Some(n);
                    break;
                }
            }
            found.ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?
        };
        let ret_inputs: Vec<NodeOutputId> =
            fg.graph.node_inputs(ret_node).into_iter().collect();
        // Return inputs = [ctrl(0), retval0(1), …]
        let retval = ret_inputs.get(1).copied().ok_or_else(|| {
            ErrorKind::AssertionFailed("Return node missing retval input".into())
        })?;
        let producer = fg.graph.get_node_from_output(retval);
        match fg.graph.node_kind(producer) {
            NodeKind::IntConst(v) => assert_eq!(*v, 3, "popcount(0b1011) == 3"),
            other => panic!("expected IntConst after rewrite, got {other:?}"),
        }
        Ok(())
    }

    /// Multi-capture rule exercising `int_binary_any` + `int_const_with!`
    /// with an op-variant capture.  Rewrites `Add(IntConst(1,U32),
    /// IntConst(2,U32))` to `IntConst(3, U32)`.
    #[test]
    fn int_const_with_int_binary_any_add() -> Result<()> {
        use crate::pat::int_binary_any;
        use crate::var::{IntBinaryOpVar, IntVar};

        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let one = b.build_int_const(1, NodeOutputType::U32);
        let two = b.build_int_const(2, NodeOutputType::U32);
        let add_out =
            b.build_int_binary_operation(one, two, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(add_out), &[])?;
        let mut fg = b.build()?;

        let add_node = fg.graph.get_node_from_output(add_out);

        let op = IntBinaryOpVar::new();
        let l = IntVar::new();
        let rr = IntVar::new();

        // A tiny evaluator: enough for Add/Sub/Mul at test-scope.
        fn eval_simple(op: IntBinaryOp, l: u64, r: u64) -> Option<u64> {
            match op {
                IntBinaryOp::Add => Some(l.wrapping_add(r)),
                IntBinaryOp::Sub => Some(l.wrapping_sub(r)),
                IntBinaryOp::Mul => Some(l.wrapping_mul(r)),
                _ => None,
            }
        }

        // Matches LHS using `any_int_const(IntVar)` to bind each operand's
        // value; both operand orderings considered automatically by the
        // commutative-match path.
        use crate::pat::any_int_const;
        let rule = rewrite_rule(
            int_binary_any(op, any_int_const(l), any_int_const(rr)),
            int_const_with!([op, l, rr, ty] => {
                // `ty` is bound by the macro to the root's output type;
                // read it to exercise the auto-binding path.
                let _ty: NodeOutputType = ty;
                eval_simple(op, l, rr).ok_or_else(|| Error::rewrite_closure(
                    std::io::Error::other("unsupported op in test evaluator"),
                ))?
            }),
        );
        let changed = rule(&mut fg, add_node)?;
        assert!(changed, "int_binary_any+Add rule should fire");

        // Locate the Return and verify its retval is IntConst(3).
        let ret_node = fg
            .preorder()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
        let ret_inputs: Vec<NodeOutputId> =
            fg.graph.node_inputs(ret_node).into_iter().collect();
        let retval = ret_inputs.get(1).copied().ok_or_else(|| {
            ErrorKind::AssertionFailed("Return node missing retval".into())
        })?;
        let producer = fg.graph.get_node_from_output(retval);
        match fg.graph.node_kind(producer) {
            NodeKind::IntConst(v) => assert_eq!(*v, 3),
            other => panic!("expected IntConst, got {other:?}"),
        }
        Ok(())
    }

    /// `bool_const_with!`: rewrites `BoolUnary(Neg, BoolConst(true))`
    /// to `BoolConst(false)`.  Exercises the `BoolVar` typed capture
    /// end-to-end.
    #[test]
    fn bool_const_with_not_rewrite() -> Result<()> {
        use crate::pat::{any_bool_const, bool_unary};
        use crate::var::BoolVar;
        use ir::BoolUnaryOp;

        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let t = b.build_boolean_const(true);
        let notted = b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)?;
        // Return directly from the Bool output — `build_return` accepts
        // any value type on the ret-val slot.
        b.build_return(Some(notted), &[])?;
        let mut fg = b.build()?;

        let not_node = fg.graph.get_node_from_output(notted);

        let bv = BoolVar::new();
        let rule = rewrite_rule(
            bool_unary(BoolUnaryOp::Neg, any_bool_const(bv)),
            bool_const_with!([bv] => !bv),
        );
        let changed = rule(&mut fg, not_node)?;
        assert!(changed, "bool_const_with Neg rule should fire");

        // The Return input should now be a BoolConst(false).
        let ret_node = fg
            .preorder()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
        let ret_inputs: Vec<NodeOutputId> =
            fg.graph.node_inputs(ret_node).into_iter().collect();
        let retval = ret_inputs.get(1).copied().ok_or_else(|| {
            ErrorKind::AssertionFailed("Return node missing retval".into())
        })?;
        let producer = fg.graph.get_node_from_output(retval);
        match fg.graph.node_kind(producer) {
            NodeKind::BoolConst(v) => assert!(!*v, "!true == false"),
            other => panic!("expected BoolConst after rewrite, got {other:?}"),
        }
        Ok(())
    }

    /// `float_const_with!`: flips the sign bit of a `FloatConst(1.0f64)`,
    /// yielding `FloatConst(-1.0f64)`.  Exercises the `FloatVar` typed
    /// capture end-to-end.
    #[test]
    fn float_const_with_signbit_flip() -> Result<()> {
        use crate::pat::any_float_const;
        use crate::var::FloatVar;

        // Minimal graph: return a FloatConst(1.0, F64).  The rule fires on
        // the `FloatConst` root directly.
        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let f_out = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
        b.build_return(Some(f_out), &[])?;
        let mut fg = b.build()?;

        let f_node = fg.graph.get_node_from_output(f_out);

        let f = FloatVar::new();
        let signbit = 0x8000_0000_0000_0000u64;
        let rule = rewrite_rule(
            any_float_const(f),
            float_const_with!([f] => f ^ signbit),
        );
        let changed = rule(&mut fg, f_node)?;
        assert!(changed, "float_const_with sign-flip rule should fire");

        // The Return input should now be FloatConst(-1.0).
        let ret_node = fg
            .preorder()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
        let ret_inputs: Vec<NodeOutputId> =
            fg.graph.node_inputs(ret_node).into_iter().collect();
        let retval = ret_inputs.get(1).copied().ok_or_else(|| {
            ErrorKind::AssertionFailed("Return node missing retval".into())
        })?;
        let producer = fg.graph.get_node_from_output(retval);
        match fg.graph.node_kind(producer) {
            NodeKind::FloatConst(bits) => {
                assert_eq!(*bits, (-1.0f64).to_bits(), "sign-bit flip of +1.0 is -1.0");
            }
            other => panic!("expected FloatConst, got {other:?}"),
        }
        Ok(())
    }

    /// A closure returning `Err(pattern::Error::rewrite_closure(...))`
    /// surfaces through the rewrite engine as `Err(_)`, not a panic.
    #[test]
    fn int_const_with_closure_error_surfaces_via_result() -> Result<()> {
        use crate::pat::{any_int_const, popcount};
        use crate::var::IntVar;

        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let c = b.build_int_const(0, NodeOutputType::U32);
        let pc_out = b.build_popcount(c, NodeOutputType::U32)?;
        b.build_return(Some(pc_out), &[])?;
        let mut fg = b.build()?;
        let pc_node = fg.graph.get_node_from_output(pc_out);

        #[derive(Debug, thiserror::Error)]
        #[error("deliberate test error")]
        struct E;

        let v = IntVar::new();
        let rule = rewrite_rule(
            popcount(any_int_const(v)),
            int_const_with!([v] => {
                // Force an error surface via `?` inside the body;
                // the body's `Result<u64>` context propagates it out.
                let _ = v;
                Err::<u64, _>(Error::rewrite_closure(E))?
            }),
        );
        let err = rule(&mut fg, pc_node).expect_err("rule should surface closure error");
        let msg = format!("{err}");
        assert!(
            msg.contains("deliberate test error") || msg.contains("rewrite-rule closure"),
            "error should mention the closure failure, got: {msg}"
        );
        Ok(())
    }

    /// A closure that returns `Err(pattern::Error::skip())` must be treated
    /// as "rule doesn't apply" by the `rewrite_rule` interpreter, not as a
    /// hard error.  The return value is `Ok(false)` and the graph is left
    /// untouched.
    #[test]
    fn int_const_with_skip_returns_ok_false() -> Result<()> {
        use crate::pat::{any_int_const, popcount};
        use crate::var::IntVar;

        let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
        let r = b.create_region()?;
        b.set_entry_region(r)?;
        b.set_region(r);
        let c = b.build_int_const(0, NodeOutputType::U32);
        let pc_out = b.build_popcount(c, NodeOutputType::U32)?;
        b.build_return(Some(pc_out), &[])?;
        let mut fg = b.build()?;
        let pc_node = fg.graph.get_node_from_output(pc_out);

        let v = IntVar::new();
        let rule = rewrite_rule(
            popcount(any_int_const(v)),
            int_const_with!([v] => {
                let _ = v;
                // Partial oracle decided the rule doesn't apply.
                None::<u64>.ok_or_else(Error::skip)?
            }),
        );
        let changed = rule(&mut fg, pc_node)?;
        assert!(
            !changed,
            "Error::skip() inside a closure should map to Ok(false)"
        );
        // Return should still point at the original popcount node.
        let ret_node = fg
            .preorder()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
            .ok_or_else(|| ErrorKind::AssertionFailed("no Return node".into()))?;
        let ret_inputs: Vec<NodeOutputId> =
            fg.graph.node_inputs(ret_node).into_iter().collect();
        let retval = ret_inputs.get(1).copied().ok_or_else(|| {
            ErrorKind::AssertionFailed("Return node missing retval".into())
        })?;
        let producer = fg.graph.get_node_from_output(retval);
        assert!(
            matches!(fg.graph.node_kind(producer), NodeKind::Popcount),
            "graph should be untouched after a skip"
        );
        Ok(())
    }

    /// `Error::skip()` is distinguishable from other error kinds via
    /// `is_skip()`, so the `rewrite_rule` interpreter can safely demultiplex
    /// them.
    #[test]
    fn error_skip_is_detectable() {
        let e = Error::skip();
        assert!(e.is_skip(), "Error::skip() should report is_skip() == true");

        let other = Error::from(ErrorKind::AssertionFailed("nope".into()));
        assert!(
            !other.is_skip(),
            "non-skip errors should report is_skip() == false"
        );
    }
}
