//! Shared crate-private `macro_rules!` helpers for collapsing constructor
//! boilerplate.
//!
//! The `pat/ctor/*.rs` modules define dozens of public one-line wrappers that
//! delegate to a family-level dispatcher.  For example, the wrapper generated
//! for `IntBinaryOp::Add` is equivalent to:
//!
//! ```rust
//! use pattern::{IntBinaryOp, IntBinaryOpPat, Pat, int_binary};
//!
//! fn add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
//!     int_binary(IntBinaryOp::Add, lhs, rhs)
//! }
//! ```
//!
//! The macros below generate such wrappers for every op variant in each
//! family (integer / boolean / float × binary / unary / cmp).  The macros
//! generate those wrappers given the name of the family-level dispatcher
//! (`int_binary`, `int_unary`, `int_cmp`, …), the op enum (`IntBinaryOp`, …),
//! and the return type (`IntBinaryOpPat` or `Pat`).
//!
//! Each constructor returns a [`crate::pat::Pat`] (or a typed builder for
//! the binary-op families).  Buildable patterns route through the
//! `with_build_*` helpers in [`crate::pat::node_pat::NodePat`] so the
//! same constructor doubles as a rewrite-rule RHS template.

/// Declare public two-operand `Pat`-side constructors that delegate to a
/// dispatcher `$builder(OpEnum::$variant, lhs, rhs)`.
///
/// Each entry is `(pub_fn_name, OpEnumVariant)` and may carry arbitrary outer
/// attributes (doc comments, `#[inline]`, …) which attach to the generated
/// function.
macro_rules! decl_pat_binary_ops {
    ($builder:ident, $op_enum:ident, $ret:ty, [ $( $(#[$attr:meta])* ($fn_name:ident, $variant:ident) ),* $(,)? ]) => {
        $(
            $(#[$attr])*
            pub fn $fn_name(lhs: impl Into<$crate::pat::Pat>, rhs: impl Into<$crate::pat::Pat>) -> $ret {
                $builder($op_enum::$variant, lhs, rhs)
            }
        )*
    };
}

/// Declare public one-operand `Pat`-side constructors that delegate to a
/// dispatcher `$builder(OpEnum::$variant, operand)`.
macro_rules! decl_pat_unary_ops {
    ($builder:ident, $op_enum:ident, $ret:ty, [ $( $(#[$attr:meta])* ($fn_name:ident, $variant:ident) ),* $(,)? ]) => {
        $(
            $(#[$attr])*
            pub fn $fn_name(operand: impl Into<$crate::pat::Pat>) -> $ret {
                $builder($op_enum::$variant, operand)
            }
        )*
    };
}

/// Declare public comparison `Pat`-side constructors.  Identical in shape to
/// [`decl_pat_binary_ops`] but kept as a distinct macro so the call-site reads
/// as a cmp-op group rather than a binary-op group.
macro_rules! decl_pat_cmp_ops {
    ($builder:ident, $op_enum:ident, $ret:ty, [ $( $(#[$attr:meta])* ($fn_name:ident, $variant:ident) ),* $(,)? ]) => {
        $(
            $(#[$attr])*
            pub fn $fn_name(lhs: impl Into<$crate::pat::Pat>, rhs: impl Into<$crate::pat::Pat>) -> $ret {
                $builder($op_enum::$variant, lhs, rhs)
            }
        )*
    };
}

pub(crate) use decl_pat_binary_ops;
pub(crate) use decl_pat_cmp_ops;
pub(crate) use decl_pat_unary_ops;

// ── *_const_with! macros ─────────────────────────────────────────────────────
//
// Rewrite-rule RHS sugar for building constants whose value depends on
// LHS-captured variables.  The macros expand to a call to
// [`crate::int_const_with_fn`] (or bool/float variants) with a closure that
// resolves each named capture against `ctx.bindings` (a [`Bindings`] view
// of the LHS match) and evaluates the body.
//
// Each entry is `name: kind`, where `kind` selects which extractor to call:
//
//   * `uint`            — `Bindings::get_uint(c, &graph)`  (`u128`)
//   * `int`             — `Bindings::get_int(c, &graph)`   (`i128`)
//   * `bool`            — `Bindings::get_bool(c, &graph)`  (`bool`)
//   * `float_bits`      — `Bindings::get_float_bits(c, &graph)` (`u64`)
//   * `int_binary_op` / `int_unary_op` / `int_cmp_op`
//   * `bool_binary_op` / `bool_unary_op`
//   * `float_binary_op` / `float_unary_op` / `float_cmp_op`
//
// Two reserved bare identifiers, if present in the bracket list, bind
// graph-derived values rather than capture lookups:
//
//   * `ty` — the root node's output type ([`crate::BuildCtx::root_ty`]).
//   * `in_ty` — `Option<NodeOutputType>`: the type of the root's first
//     value input.  Use `in_ty.ok_or_else(pattern::skip)?` if required.
//
// Missing bindings raise [`crate::error::MissingBinding`] tagged with the
// macro entry's `kind` token (e.g. `"uint"`, `"int_binary_op"`).

/// Builds an `IntConst` node whose value is computed from LHS captures.
#[macro_export]
macro_rules! int_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::int_const_with_fn(move |__strider_ctx: &$crate::BuildCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Builds a `BoolConst` node whose value is computed from LHS captures.
#[macro_export]
macro_rules! bool_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::bool_const_with_fn(move |__strider_ctx: &$crate::BuildCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Builds a `FloatConst` node whose IEEE 754 bit pattern is computed from
/// LHS captures.  The body must evaluate to `u64`.
#[macro_export]
macro_rules! float_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::float_const_with_fn(move |__strider_ctx: &$crate::BuildCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
    };
}

/// Internal helper: tt-muncher that expands a capture list into `let`
/// bindings inside the `*_const_with!` closure body.
///
/// Accepts entries of the form:
///   * `ty`              — bare ident, graph-derived (matched by spelling
///                          via the inner [`__const_with_bind_one`])
///   * `in_ty`           — bare ident, graph-derived (same)
///   * `name: kind`      — typed capture extraction
///
/// Hygiene: each emitted `let` is built by [`__const_with_bind_one`]
/// from a `$hy:ident` that was bound from the caller's token, so the
/// surrounding closure body sees the caller's identifier directly.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bindings {
    ($ctx:ident;) => {};
    // Bare ident — `ty` / `in_ty` (dispatched by spelling inside
    // `__const_with_bind_one`).
    ($ctx:ident; $cap:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
    // Typed capture: `name: kind`.
    ($ctx:ident; $name:ident : $kind:ident $(, $($rest:tt)*)?) => {
        let $name = $crate::__const_with_extract!($ctx, $name, $kind)?;
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
}

/// Internal helper: emits a single `let`-binding for the bare-ident
/// form (`ty` / `in_ty`) of the `*_const_with!` capture list,
/// dispatching by ident *spelling*.  The third argument is the
/// caller's literal identifier (passed twice from
/// [`__const_with_bindings`]) so the emitted `let` lives in the
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

/// Internal helper: maps a `kind` token to the right `Bindings` extractor
/// call.  Each arm returns an `anyhow::Result<T>` so the surrounding
/// closure body can use the `?` operator uniformly.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_extract {
    ($ctx:ident, $cap:ident, uint) => {
        $ctx.bindings
            .get_uint($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("uint"))
    };
    ($ctx:ident, $cap:ident, int) => {
        $ctx.bindings
            .get_int($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("int"))
    };
    ($ctx:ident, $cap:ident, bool) => {
        $ctx.bindings
            .get_bool($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("bool"))
    };
    ($ctx:ident, $cap:ident, float_bits) => {
        $ctx.bindings
            .get_float_bits($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("float_bits"))
    };
    ($ctx:ident, $cap:ident, int_binary_op) => {
        $ctx.bindings
            .get_int_binary_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("int_binary_op"))
    };
    ($ctx:ident, $cap:ident, int_unary_op) => {
        $ctx.bindings
            .get_int_unary_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("int_unary_op"))
    };
    ($ctx:ident, $cap:ident, int_cmp_op) => {
        $ctx.bindings
            .get_int_cmp_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("int_cmp_op"))
    };
    ($ctx:ident, $cap:ident, bool_binary_op) => {
        $ctx.bindings
            .get_bool_binary_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("bool_binary_op"))
    };
    ($ctx:ident, $cap:ident, bool_unary_op) => {
        $ctx.bindings
            .get_bool_unary_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("bool_unary_op"))
    };
    ($ctx:ident, $cap:ident, float_binary_op) => {
        $ctx.bindings
            .get_float_binary_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("float_binary_op"))
    };
    ($ctx:ident, $cap:ident, float_unary_op) => {
        $ctx.bindings
            .get_float_unary_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("float_unary_op"))
    };
    ($ctx:ident, $cap:ident, float_cmp_op) => {
        $ctx.bindings
            .get_float_cmp_op($cap, &*$ctx.graph)
            .ok_or_else(|| $crate::__missing_binding("float_cmp_op"))
    };
}
