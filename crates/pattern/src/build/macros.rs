//! The `*_const_with!` macros and the `first_value_input_type` helper they
//! depend on.
//!
//! The three user-facing macros ([`crate::int_const_with!`],
//! [`crate::bool_const_with!`], [`crate::float_const_with!`]) and the two
//! internal tt-munchers ([`crate::__const_with_bindings!`],
//! [`crate::__const_with_bind_one!`]) all expand using `$crate::build::…`
//! paths, so relocating this file inside the `build/` subtree does not break
//! them: `pattern::build::{int_const_fn, bool_const_fn, float_const_fn,
//! BuildCtx, FromCtx, first_value_input_type}` all remain reachable via the
//! `build/mod.rs` re-exports.

use ir::node::{NodeOutputKind, NodeOutputType};

use super::BuildCtx;

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

/// Builds an `IntConst` node whose value is computed from LHS captures.
///
/// # Syntax
///
/// ```text
/// int_const_with!([cap1, cap2, ...] => expression)
/// ```
///
/// Each `capN` is a capture identifier that also appears in the LHS pattern.
/// The macro expands to an [`crate::build::int_const_fn`] closure that binds
/// each capture to its concrete value via [`crate::build::FromCtx`] and
/// evaluates the body, wrapping the result in `Ok`.
///
/// Two special identifiers, if present in the bracket list, are bound to
/// graph-derived values rather than looked up via [`crate::build::FromCtx`]:
///
/// * `ty` — the root node's output type ([`crate::build::BuildCtx::root_ty`]).
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
/// [`crate::build::FromCtx::from_ctx`], which returns `Result<_>`; the
/// surrounding closure body uses `?` to propagate a missing binding as an
/// error.
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
