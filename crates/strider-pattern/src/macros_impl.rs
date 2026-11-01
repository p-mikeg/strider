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
//!   * `uint`: `Bindings::get_uint`, yielding `UBig`.
//!   * `float_bits`: `Bindings::get_float_bits`, yielding `u64`.
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
//! A missing binding raises [`missing_binding`](crate::missing_binding) tagged
//! with the entry's `kind` token. Writing the kind `kind?` instead yields an
//! `Option` and no error, for a capture an alternation binds on only some
//! arms; the rewrite coverage check is relaxed to match, so an entry is
//! spelled `?` only where the body really handles `None`.

/// Bracket-list grammar is in the module docs.
#[macro_export]
macro_rules! int_const_with {
    ($($t:tt)*) => { $crate::__const_with!(int_const_with_fn; $($t)*) };
}

/// Builds an `IntConst` typed `I1`.
#[macro_export]
macro_rules! bool_const_with {
    ($($t:tt)*) => { $crate::__const_with!(bool_const_with_fn; $($t)*) };
}

/// The body must evaluate to a `u64` IEEE 754 bit pattern.
#[macro_export]
macro_rules! float_const_with {
    ($($t:tt)*) => { $crate::__const_with!(float_const_with_fn; $($t)*) };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __const_with {
    ($f:ident; [$($caps:tt)*] => $body:expr) => {
        $crate::__const_with_decl!(
            $crate::$f(move |__strider_ctx: &$crate::TemplateCtx<'_>| {
                $crate::__const_with_bindings!(__strider_ctx; $($caps)*);
                Ok({ $body })
            });
            $($caps)*
        )
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
    // `name: kind?`: `None` where the binding is absent.
    ($ctx:ident; $name:ident : $kind:ident ? $(, $($rest:tt)*)?) => {
        let $name = $crate::__const_with_extract!($ctx, $name, $kind).ok();
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
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

/// Declares each `name: kind` entry of a capture list on the built
/// constant, a `kind?` one as optional, in source order; the bare
/// graph-derived idents declare nothing.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_decl {
    ($e:expr;) => {
        $e
    };
    ($e:expr; $name:ident : $kind:ident ? $(, $($rest:tt)*)?) => {
        $crate::__const_with_decl!($e.declare_optional($name); $($($rest)*)?)
    };
    ($e:expr; $name:ident : $kind:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_decl!($e.declare($name); $($($rest)*)?)
    };
    ($e:expr; $cap:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_decl!($e; $($($rest)*)?)
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
    ($ctx:ident, $cap:ident, float_bits) => {
        $ctx.bindings
            .get_float_bits($cap, $ctx.function.graph())
            .ok_or_else(|| $crate::missing_binding("float_bits"))
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
