//! `int_const_with!`, `bool_const_with!` and `float_const_with!`: the `*_const_with_fn` builder
//! with a closure that resolves each listed LHS capture against [`Bindings`](crate::Bindings),
//! then evaluates the body.
//!
//! Each list entry is `name: kind`:
//!
//!   * `uint`: `Bindings::get_uint`, yielding `UBig`.
//!   * `float_bits`: `Bindings::get_float_bits`, yielding `u64`.
//!   * `float_binary_op` / `float_unary_op` / `float_cmp_op`
//!
//! Two reserved bare identifiers:
//!
//!   * `ty`: the constant's own output type ([`crate::TemplateCtx::root_ty`]).
//!   * `in_ty`: `Option<ValueType>`, the type at the LHS root's input slot 0 if it is a typed
//!     value.
//!
//! A missing binding raises [`missing_binding`](crate::missing_binding). `kind?` yields an
//! `Option` instead and relaxes the rewrite coverage check, for a capture only some alternation
//! arms bind.

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

/// Expands a capture list into `let` bindings in the closure body.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_bindings {
    ($ctx:ident;) => {};
    ($ctx:ident; $name:ident : $kind:ident ? $(, $($rest:tt)*)?) => {
        let $name = $crate::__const_with_extract!($ctx, $name, $kind);
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
    ($ctx:ident; $name:ident : $kind:ident $(, $($rest:tt)*)?) => {
        let $name = $crate::__const_with_extract!($ctx, $name, $kind)
            .ok_or_else(|| $crate::missing_binding(stringify!($kind)))?;
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
    // Bare `ty` / `in_ty`, dispatched by spelling in `__const_with_bind_one`.
    ($ctx:ident; $cap:ident $(, $($rest:tt)*)?) => {
        $crate::__const_with_bind_one!($ctx, $cap, $cap);
        $( $crate::__const_with_bindings!($ctx; $($rest)*); )?
    };
}

/// Declares each `name: kind` entry on the built constant (`kind?` as optional).
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

/// The `let` for a bare `ty` / `in_ty`. `$hy` is the caller's own token, so the binding is
/// visible in the caller's body (hygiene).
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

/// Maps a `kind` token to its `Bindings` extractor, as `Option<T>`.
#[doc(hidden)]
#[macro_export]
macro_rules! __const_with_extract {
    ($ctx:ident, $cap:ident, uint) => {
        $ctx.bindings.get_uint($cap, $ctx.function)
    };
    ($ctx:ident, $cap:ident, float_bits) => {
        $ctx.bindings.get_float_bits($cap, $ctx.function.graph())
    };
    ($ctx:ident, $cap:ident, float_binary_op) => {
        $ctx.bindings
            .get_float_binary_op($cap, $ctx.function.graph())
    };
    ($ctx:ident, $cap:ident, float_unary_op) => {
        $ctx.bindings
            .get_float_unary_op($cap, $ctx.function.graph())
    };
    ($ctx:ident, $cap:ident, float_cmp_op) => {
        $ctx.bindings.get_float_cmp_op($cap, $ctx.function.graph())
    };
}
