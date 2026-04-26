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
//! Build a trivial function that loads from `base + offset`, then match the
//! `Load` and extract its two address operands.
//!
//! ```rust
//! use ir::{FunctionBuilder, IntBinaryOp, node::NodeOutputType};
//! use pattern::{Matcher, Var, add, load, var};
//!
//! // *(0x1000 + 8); return the loaded value.
//! let mut fb = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
//! let region = fb.create_region().unwrap();
//! fb.set_entry_region(region).unwrap();
//! fb.set_region(region);
//! let base = fb.build_int_const(0x1000u64, NodeOutputType::U64);
//! let offset = fb.build_int_const(8u64, NodeOutputType::U64);
//! let addr = fb
//!     .build_int_binary_operation(base, offset, IntBinaryOp::Add, NodeOutputType::U64)
//!     .unwrap();
//! let val = fb.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64).unwrap();
//! fb.build_return(Some(val), &[]).unwrap();
//! let graph = fb.build().unwrap();
//!
//! // Match every load whose address is (something + anything).
//! let ptr_v = Var::new();
//! let off_v = Var::new();
//! let pat = load().addr(add(var(ptr_v), var(off_v)));
//!
//! let matcher = Matcher::new(&graph);
//! let hits = matcher.find_all(&pat.into());
//! assert_eq!(hits.len(), 1);
//!
//! // The captured operands are `NodeOutputId`s; `get_int_const` resolves
//! // them to the concrete constant values their producers yielded.
//! let m = &hits[0];
//! assert_eq!(m.get_int_const(ptr_v, &graph), Some(0x1000u128));
//! assert_eq!(m.get_int_const(off_v, &graph), Some(8u128));
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
//!
//! ## Walk-through flags ([`MatcherOptions`])
//!
//! The matcher's default semantics are **strict exact-walk** — every input
//! position must match the pattern there directly.  Two opt-in flags relax
//! this when register-merge / width-cast noise from the lifter / optimizer
//! makes patterns brittle:
//!
//! * [`Matcher::ignore_casts`] — walk through value-passthrough cast nodes
//!   (`Extend`, `Truncate`, `CastToInt`, `CastToFloat`, `CastToBool`,
//!   `IntBitsToFloat`, `FloatBitsToInt`) when matching value inputs.
//!   Lets `add(mul(_,_), _)` find `Add(Extend(Mul), arg)` without re-shaping
//!   the source.
//! * [`Matcher::ignore_control_states`] — walk through `ControlState`
//!   (region-join) nodes when traversing control chains.  Lets
//!   `ret(call(...))` cross region joins between the Return and the Call.
//!
//! Both default to off; both are sticky on the matcher instance.  Direct
//! match is always tried first, so strict patterns (e.g. `truncate(x)`
//! looking for a literal `Truncate`) keep working unchanged.
//!
//! ```rust,ignore
//! let m = Matcher::new(&graph)
//!     .ignore_casts()
//!     .ignore_control_states();
//! ```

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

pub use matcher::{Bindings, Match, Matcher, MatcherOptions};
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
