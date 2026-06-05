//! Crate-public `macro_rules!` helpers for collapsing rewrite-RHS
//! constant-from-bindings boilerplate.
//!
//! Ported from `strider-orchestrator::pattern::macros` so consumers can
//! depend on a single source of truth.  Three public macros —
//! [`int_const_with!`], [`bool_const_with!`], [`float_const_with!`] —
//! expand to a call to the matching `*_const_with_fn` builder with a
//! closure that resolves each named LHS capture against
//! [`Bindings`](crate::Bindings) and evaluates the body.
//!
//! Each entry inside the macro's bracket list is `name: kind`, where
//! `kind` selects which extractor to call:
//!
//!   * `uint`            — `Bindings::get_uint(c, &graph)`  (`u128`)
//!   * `int`             — `Bindings::get_int(c, &graph)`   (`i128`)
//!   * `bool`            — `Bindings::get_bool(c, &graph)`  (`bool`)
//!   * `float_bits`      — `Bindings::get_float_bits(c, &graph)` (`u64`)
//!   * `int_binary_op` / `int_unary_op` / `int_cmp_op`
//!   * `bool_binary_op`
//!   * `float_binary_op` / `float_unary_op` / `float_cmp_op`
//!
//! Two reserved bare identifiers, if present in the bracket list,
//! bind graph-derived values rather than capture lookups:
//!
//!   * `ty`    — the rewrite root's output type
//!     ([`crate::TemplateCtx::root_ty`]).
//!   * `in_ty` — `Option<ValueType>`: the type of the rewrite
//!     root's first value input.  Use
//!     `in_ty.ok_or_else(strider_pattern::skip)?` if required.
//!
//! Missing bindings raise a [`MissingBinding`](crate::MissingBinding)
//! error tagged with the macro entry's `kind` token.

/// Builds an `IntConst` node whose value is computed from LHS
/// captures.  See module docs for the bracket-list grammar.
#[macro_export]
macro_rules! int_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::int_const_with_fn(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Builds an `I1` boolean constant (an `IntConst` typed `I1`) whose
/// value is computed from LHS captures.
#[macro_export]
macro_rules! bool_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::bool_const_with_fn(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed
/// from LHS captures.  The body must evaluate to `u64`.
#[macro_export]
macro_rules! float_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::float_const_with_fn(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Internal helper: tt-muncher that expands a capture list into
/// `let` bindings inside the `*_const_with!` closure body.
///
/// Accepts entries of the form:
///   * `ty`              — bare ident, graph-derived (matched by
///                          spelling via the inner
///                          [`__const_with_bind_one`])
///   * `in_ty`           — bare ident, graph-derived (same)
///   * `name: kind`      — typed capture extraction
///
/// Hygiene: each emitted `let` is built by `__const_with_bind_one`
/// from a `$hy:ident` that was bound from the caller's token, so the
/// surrounding closure body sees the caller's identifier directly.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bindings {
    ($ctx:ident;) => {};
    // Typed capture: `name: kind`.
    ($ctx:ident; $name:ident : $kind:ident $(, $($rest:tt)*)?) => {
        let $name = $crate::__const_with_extract!($ctx, $name, $kind)?;
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
    // Bare ident — `ty` / `in_ty` (dispatched by spelling inside
    // `__const_with_bind_one`).
    ($ctx:ident; $cap:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
}

/// Internal helper: emits a single `let`-binding for the bare-ident
/// form (`ty` / `in_ty`) of the `*_const_with!` capture list,
/// dispatching by ident *spelling*.  The third argument is the
/// caller's literal identifier (passed twice from
/// `__const_with_bindings`) so the emitted `let` lives in the
/// caller's hygiene context.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bind_one {
    ($ctx:ident, ty, $hy:ident) => {
        let $hy = $ctx.root_ty;
        let _ = &$hy;
    };
    ($ctx:ident, in_ty, $hy:ident) => {
        let $hy = $crate::first_value_input_type($ctx);
        let _ = &$hy;
    };
}

/// Internal helper: maps a `kind` token to the right `Bindings`
/// extractor call.  Each arm returns an `anyhow::Result<T>` so the
/// surrounding closure body can use the `?` operator uniformly.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_extract {
    ($ctx:ident, $cap:ident, uint) => {
        $ctx.bindings
            .get_uint($cap, $ctx.function)
            .ok_or_else(|| $crate::missing_binding("uint"))
    };
    ($ctx:ident, $cap:ident, int) => {
        $ctx.bindings
            .get_int($cap, $ctx.function)
            .ok_or_else(|| $crate::missing_binding("int"))
    };
    ($ctx:ident, $cap:ident, bool) => {
        $ctx.bindings
            .get_bool($cap, $ctx.function)
            .ok_or_else(|| $crate::missing_binding("bool"))
    };
    ($ctx:ident, $cap:ident, float_bits) => {
        $ctx.bindings
            .get_float_bits($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("float_bits"))
    };
    ($ctx:ident, $cap:ident, int_binary_op) => {
        $ctx.bindings
            .get_int_binary_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("int_binary_op"))
    };
    ($ctx:ident, $cap:ident, int_unary_op) => {
        $ctx.bindings
            .get_int_unary_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("int_unary_op"))
    };
    ($ctx:ident, $cap:ident, int_cmp_op) => {
        $ctx.bindings
            .get_int_cmp_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("int_cmp_op"))
    };
    ($ctx:ident, $cap:ident, bool_binary_op) => {
        $ctx.bindings
            .get_bool_binary_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("bool_binary_op"))
    };
    ($ctx:ident, $cap:ident, float_binary_op) => {
        $ctx.bindings
            .get_float_binary_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("float_binary_op"))
    };
    ($ctx:ident, $cap:ident, float_unary_op) => {
        $ctx.bindings
            .get_float_unary_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("float_unary_op"))
    };
    ($ctx:ident, $cap:ident, float_cmp_op) => {
        $ctx.bindings
            .get_float_cmp_op($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("float_cmp_op"))
    };
}
