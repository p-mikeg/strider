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
//! family (integer / boolean / float × binary / unary / cmp).  The macros below generate those wrappers given the name of
//! the family-level dispatcher (`int_binary`, `int_unary`, `int_cmp`, …), the
//! op enum (`IntBinaryOp`, …), and the return type (`IntBinaryOpPat` or `Pat`).
//!
//! The shape intentionally mirrors — and is named the same as — the analogous
//! macros in [`crate::build::constructors`], which already collapse the same
//! boilerplate on the `Build`-typed side.  Keeping the two sides structurally
//! parallel makes it easy to tell at a glance that the pat-side and build-side
//! wrappers stay in lock-step.

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
// resolves each named capture via [`crate::FromCtx`] and evaluates the body.
//
// Two reserved identifiers, if present in the bracket list, are bound to
// graph-derived values rather than looked up via `FromCtx`:
//
// * `ty` — the root node's output type ([`crate::BuildCtx::root_ty`]).
// * `in_ty` — `Option<NodeOutputType>`: the type of the root's first value
//   input.  Use `in_ty.ok_or_else(pattern::Error::skip)?` if required.

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
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bindings {
    ($ctx:ident;) => {};
    ($ctx:ident; $cap:ident $(,)?) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
    };
    ($ctx:ident; $cap:ident, $($rest:tt)*) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
        $crate::__const_with_bindings!($ctx; $($rest)*);
    };
}

/// Internal helper: emits a single `let`-binding, dispatching by ident
/// *spelling*: `ty` / `in_ty` bind graph-derived values; anything else
/// falls back to `FromCtx::from_ctx`.
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
    ($ctx:ident, $_sel:ident, $hy:ident) => {
        let $hy = $crate::FromCtx::from_ctx(&$hy, $ctx)?;
    };
}
