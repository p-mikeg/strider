//! Collapses rewrite-RHS constant-from-bindings boilerplate.
//!
//! `int_const_with!`, `bool_const_with!` and `float_const_with!` expand
//! to the matching `*_const_with_fn` builder with a closure that resolves each
//! named LHS capture against [`Bindings`](crate::Bindings), then evaluates the
//! body.
//!
//! Bracket-list grammar. Each entry is `name: kind`, where `kind` picks the
//! extractor:
//!
//!   * `uint`, `int`, `bool`: `Bindings::get_uint` / `get_int` / `get_bool`,
//!     yielding `u128` / `i128` / `bool`.
//!   * `float_bits`: `Bindings::get_float_bits`, yielding `u64`.
//!   * `int_binary_op` / `int_unary_op` / `int_cmp_op`
//!   * `bool_binary_op`
//!   * `float_binary_op` / `float_unary_op` / `float_cmp_op`
//!
//! Two reserved bare identifiers bind graph-derived values instead of capture
//! lookups:
//!
//!   * `ty`: the output type resolved for the constant's own node
//!     ([`crate::TemplateCtx::root_ty`]): the rewrite root's type where the
//!     node inherits it, `I1` under `bool_const_with!`.
//!   * `in_ty`: `Option<ValueType>`, the type at the LHS root's input slot 0,
//!     `None` unless that input is a typed value. Use
//!     `in_ty.ok_or_else(strider_pattern::skip)?` where it is required.
//!
//! A missing binding raises [`MissingBinding`](crate::MissingBinding) tagged
//! with the entry's `kind` token.

/// Bracket-list grammar is in the module docs.
#[macro_export]
macro_rules! int_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::int_const_with_fn(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
        .declaring($crate::__const_with_caps!($($caps)*))
    };
}

/// Builds an `IntConst` typed `I1`.
#[macro_export]
macro_rules! bool_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::bool_const_with_fn(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
        .declaring($crate::__const_with_caps!($($caps)*))
    };
}

/// The body must evaluate to a `u64` IEEE 754 bit pattern.
#[macro_export]
macro_rules! float_const_with {
    ([$($caps:tt)*] => $body:expr) => {
        $crate::float_const_with_fn(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
            $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
            Ok({ $body })
        })
        .declaring($crate::__const_with_caps!($($caps)*))
    };
}

/// tt-muncher expanding a capture list into `let` bindings inside the
/// `*_const_with!` closure body.
///
/// Hygiene: each emitted `let` is built by `__const_with_bind_one` from a
/// `$hy:ident` bound from the caller's own token, so the closure body sees the
/// caller's identifier.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bindings {
    ($ctx:ident;) => {};
    ($ctx:ident; $name:ident : $kind:ident $(, $($rest:tt)*)?) => {
        let $name = $crate::__const_with_extract!($ctx, $name, $kind)?;
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
    // Bare `ty` / `in_ty`, dispatched by spelling in `__const_with_bind_one`.
    ($ctx:ident; $cap:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
}

/// Collects the `name: kind` entries of a capture list into a `Vec<Capture>`,
/// skipping the bare graph-derived idents. Source order, so the declaration
/// reads like the list.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_caps {
    () => {
        ::std::vec::Vec::new()
    };
    ($name:ident : $kind:ident $(, $($rest:tt)*)?) => {{
        let mut caps: ::std::vec::Vec<$crate::Capture> =
            $crate::__const_with_caps!($($($rest)*)?);
        caps.insert(0, $name);
        caps
    }};
    ($cap:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_caps!($($($rest)*)?)
    };
}

/// Emits the `let` for a bare-ident capture entry, dispatching on ident
/// *spelling*. The third argument is the caller's literal identifier, passed
/// twice from `__const_with_bindings`, so the `let` lands in the caller's
/// hygiene context.
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

/// Maps a `kind` token to its `Bindings` extractor. Every arm yields
/// `anyhow::Result<T>` so the closure body can `?` uniformly.
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
