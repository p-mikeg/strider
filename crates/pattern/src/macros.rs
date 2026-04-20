//! Shared crate-private `macro_rules!` helpers for collapsing constructor
//! boilerplate.
//!
//! The `pat/ctor/*.rs` modules define dozens of public one-line wrappers of
//! the form
//!
//! ```ignore
//! pub fn add(lhs: impl Into<Pat>, rhs: impl Into<Pat>) -> IntBinaryOpPat {
//!     int_binary(IntBinaryOp::Add, lhs, rhs)
//! }
//! ```
//!
//! for every op variant in each family (integer / boolean / float × binary /
//! unary / cmp).  The macros below generate those wrappers given the name of
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
