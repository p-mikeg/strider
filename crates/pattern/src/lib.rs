#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! IR graph pattern matching for the Strider binary analysis framework.
//!
//! This crate lets you query a lifted function's IR graph using typed patterns
//! with named captures.  A pattern describes a structural constraint on a
//! subgraph; the [`Matcher`] finds every site in the graph where it holds and
//! returns [`Match`] objects containing the captured values.
//!
//! # Quick example
//!
//! ```rust,ignore
//! use pattern::{Matcher, Var, load, add, var};
//!
//! let ptr    = Var::new();
//! let offset = Var::new();
//!
//! // Match every load whose address is (something + anything).
//! let pat = load().addr(add(var(ptr), var(offset)));
//!
//! let matcher = Matcher::new(&fn_graph);
//! for m in matcher.find_all(&pat.into()) {
//!     println!("load base: {:?}, offset: {:?}", m.get(ptr), m.get(offset));
//! }
//! ```
//!
//! # Key types
//!
//! - [`Pat`] — pattern values (cheap to clone, reference-counted)
//! - [`Var`] / [`NodeVar`] — capture variables for data outputs / control nodes
//! - [`Matcher`] — executes a pattern against an [`ir::BuiltFunctionGraph`]
//! - [`Match`] — result of one successful match; exposes captured bindings
//!
//! # Builder API
//!
//! Free functions like [`load`], [`add`], [`call`], [`if_node`] return builder
//! values that convert to [`Pat`] via `Into<Pat>`.  All field methods
//! (`.addr()`, `.arg()`, `.cond()`, …) accept `impl Into<Pat>` so builders
//! compose directly without explicit `.into()` calls.
//!
//! ## Captures and predicates
//!
//! [`var`]`(v)` is shorthand for [`any`]`().capture(v)`.  Any builder or
//! [`Pat`] supports `.capture(v)` to bind the matched output and `.when(f)` to
//! add a custom predicate guard.  The standalone [`predicate`]`(f)` constructor
//! is equivalent to [`any`]`().when(f)`.
//!
//! ## Commutative matching
//!
//! Binary operations that are mathematically commutative (`add`, `mul`, `and`,
//! `or`, `xor`, `bool_and`, `bool_or`, `bool_xor`) automatically try both
//! operand orderings.  Call `.ordered()` on the returned builder to opt out.

pub mod error;
pub use error::{Error, ErrorKind, Result};

mod macros;
mod matcher;
mod pat;
mod rewrite;
mod var;

pub use rewrite::{BoxedRule, apply_rules_in_order, boxed_rule, rewrite_rule};
pub use pat::traits::{BuildCtx, BuildOutcome};
pub use pat::ctor::consts::{FromCtx, first_value_input_type};
pub use pat::ctor::consts::{bool_const_with_fn, float_const_with_fn, int_const_with_fn};

// ── Core types & entry points ────────────────────────────────────────────────

pub use matcher::{Bindings, Match, Matcher};
pub use pat::{IntoPat, MatchPredicateFn, Pat, PredicateFn};

// ── Capture variables ────────────────────────────────────────────────────────

pub use var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

// ── Builder structs ──────────────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    BoolBinaryOpPat, CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat,
    IfPat, IntBinaryOpPat, LoadPat, PhiPat, RetPat, StackStorePat,
    StackStorePhiPat, StorePat,
};

// ── Const-capture overload traits ────────────────────────────────────────────

pub use pat::{IntoAnyBoolConst, IntoAnyFloatConst, IntoAnyIntConst};

// ── Wildcards, captures, predicates ──────────────────────────────────────────

pub use pat::{any, predicate, var};

// ── Int ops (binary, unary, comparison, variant-agnostic) ────────────────────

#[rustfmt::skip]
pub use pat::{
    add, and, div, int_binary, int_binary_any, int_carry, int_cmp, int_cmp_any,
    int_eq, int_le, int_lt, int_sborrow, int_scarry, int_sle, int_slt,
    int_unary, int_unary_any, lzcount, mul, neg, not, or, popcount,
    rem, sdiv, shl, shr, srem, sshr, sub, xor,
};

// ── Bool ops ─────────────────────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    bool_and, bool_binary, bool_binary_any, bool_not, bool_or, bool_unary,
    bool_unary_any, bool_xor,
};

// ── Float ops ────────────────────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    float_abs, float_add, float_binary, float_binary_any, float_ceil, float_cmp,
    float_cmp_any, float_div, float_eq, float_floor, float_le, float_lt,
    float_mul, float_ne, float_neg, float_round, float_sqrt, float_sub,
    float_unary, float_unary_any,
};

// ── Casts & coercions ────────────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    cast_to_bool, cast_to_float, cast_to_int, extend, float_bits_to_int,
    float_to_float, float_to_int, int_bits_to_float, int_to_float, sign_extend,
    truncate, zero_extend,
};

// ── Constants ────────────────────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    any_bool_const, any_float_const, any_int_const,
    bool_const, float_const, int_const,
};

// ── Memory, phi, function-arg ────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    function_arg, function_arg_any, function_arg_reg, function_arg_stack,
    load, phi, phi_for, stack_store, stack_store_phi, store,
};

// ── Control flow & entry values ──────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    call, call_other, if_node, initial_var, initial_var_for, ret,
};

// ── Op enums re-exported from `ir` for builder call-site convenience ────────
//
// Lets callers write `int_binary(IntBinaryOp::Add, …)` without pulling in the
// `ir` crate directly.

pub use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
