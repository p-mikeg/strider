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
//! use strider_ir::{FunctionBuilder, IntBinaryOp, node::NodeOutputType};
//! use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
//! use strider_analyze::pattern::{Capture, Matcher, add, load, var};
//!
//! // *(0x1000 + 8); return the loaded value.
//! let mut fb = FunctionBuilder::empty().unwrap();
//! let region = fb.create_region().unwrap();
//! fb.set_entry_region(region).unwrap();
//! fb.set_region(region);
//! fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
//! let base = fb.build_int_const(0x1000u64, NodeOutputType::U64).unwrap();
//! let offset = fb.build_int_const(8u64, NodeOutputType::U64).unwrap();
//! let addr = fb
//!     .build_int_binary_operation(base, offset, IntBinaryOp::Add, NodeOutputType::U64)
//!     .unwrap();
//! let val = fb.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64).unwrap();
//! fb.build_return(Some(val), &[]).unwrap();
//! fb.set_lift_addr(None);
//! let graph = fb.build().unwrap();
//!
//! // Match every load whose address is (something + anything).
//! let ptr_c = Capture::new();
//! let off_c = Capture::new();
//! let pat = load().addr(add(var(ptr_c), var(off_c)));
//!
//! let matcher = Matcher::try_new(&graph).unwrap();
//! let hits = matcher.find_all(&pat.into());
//! assert_eq!(hits.len(), 1);
//!
//! // The captured operands are `NodeOutputId`s; `get_uint` resolves
//! // them to the concrete constant values their producers yielded.
//! let m = &hits[0];
//! assert_eq!(m.get_uint(ptr_c, &graph), Some(0x1000u128));
//! assert_eq!(m.get_uint(off_c, &graph), Some(8u128));
//! ```
//!
//! # Key types
//!
//! - [`Pat`] — pattern values (cheap to clone, reference-counted)
//! - [`Capture`] — unified capture variable; binds a node id and (when
//!   the pattern is value-producing) a value output
//! - [`Matcher`] — executes a pattern against an [`strider_ir::Graph`]
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
//! [`var`]`(c)` is shorthand for [`any`]`().capture(c)`.  Any builder or
//! [`Pat`] supports `.capture(c)` to bind the matched node and `.when(f)` to
//! add a custom predicate guard.
//!
//! ## Commutative matching
//!
//! Binary operations that are mathematically commutative (`add`, `mul`, `and`,
//! `or`, `xor`, `bool_and`, `bool_or`, `bool_xor`, `float_add`, `float_mul`)
//! automatically try both operand orderings.  Symmetric integer comparisons
//! (`int_eq`, `int_carry`, `int_scarry`) and `float_eq` likewise retry with
//! swapped operands.  `float_ne` is **not** primitive: the lifter lowers
//! `FLOAT_NOTEQUAL(a, b)` to `BoolNeg(FloatEqual(a, b))`, and the inner
//! `FloatEqual` is what's auto-commutative — `float_ne` inherits the
//! commutativity through that lowering.  Variant-agnostic
//! constructors (`int_binary_any`, `bool_binary_any`, `float_binary_any`,
//! `int_cmp_any`, `float_cmp_any`) inspect the matched op and apply the
//! same rule per-match.  Call `.ordered()` on the returned builder to opt
//! out — only the typed binary-op builders (`IntBinaryOpPat`,
//! `BoolBinaryOpPat`, `FloatBinaryOpPat`) expose `.ordered()`; the bare
//! `int_cmp` / `float_cmp` ctors return a `Pat` directly.
//!
//! ## Walk-through flags (`MatcherOptions`)
//!
//! The matcher's default semantics are **strict exact-walk** — every input
//! position must match the pattern there directly.  Two opt-in flags relax
//! this when register-merge / width-cast noise from the lifter / optimizer
//! makes patterns brittle:
//!
//! * [`Matcher::ignore_casts`] — walk through every value-passthrough
//!   cast (`Extend`, `Truncate`, `CastToInt`, `CastToFloat`,
//!   `CastToBool`, `IntBitsToFloat`, `FloatBitsToInt`) when matching
//!   value inputs.  Equivalent to
//!   `.ignore_casts_mask(CastMask::all())`.  Lets `add(mul(_,_), _)`
//!   find `Add(Extend(Mul), arg)` without re-shaping the source.
//! * [`Matcher::ignore_casts_mask`] — selective version: walks through
//!   only the cast kinds whose [`CastMask`] bits are set.  Multiple
//!   calls union.  Use this when you want to skip e.g. width casts but
//!   not bitcasts.
//! * [`Matcher::ignore_regions`] — walk through `Region`
//!   (region-join) nodes when traversing control chains.  Lets
//!   `ret(call(...))` cross region joins between the Return and the Call.
//!
//! Both default to off; both are sticky on the matcher instance.  Direct
//! match is always tried first, so strict patterns (e.g. `truncate(x)`
//! looking for a literal `Truncate`) keep working unchanged.
//!
//! ```rust
//! use strider_ir::FunctionBuilder;
//! use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
//! use strider_analyze::pattern::Matcher;
//!
//! let mut fb = FunctionBuilder::empty().unwrap();
//! let region = fb.create_region().unwrap();
//! fb.set_entry_region(region).unwrap();
//! fb.set_region(region);
//! fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
//! fb.build_return(None, &[]).unwrap();
//! fb.set_lift_addr(None);
//! let graph = fb.build().unwrap();
//!
//! let m = Matcher::try_new(&graph).unwrap()
//!     .ignore_casts()
//!     .ignore_regions();
//! # let _ = m;
//! ```

pub mod error;
pub use error::Result;
pub(crate) use error::skip;
#[doc(hidden)]
pub(crate) use error::__missing_binding;

pub(crate) mod macros;
mod matcher;
mod pat;
mod rewrite;
mod var;

pub use rewrite::{
    BoxedRule, GraphRewriteCtxExt, RewriteCtx, RewriteCtxView, boxed_rule, rewrite_rule,
};
pub(crate) use rewrite::apply_rules_in_order;
pub(crate) use pat::traits::BuildCtx;
pub(crate) use pat::ctor::consts::first_value_input_type;
pub(crate) use pat::ctor::consts::{bool_const_with_fn, float_const_with_fn, int_const_with_fn};

// ── Core types & entry points ────────────────────────────────────────────────

pub use matcher::{ArgSource, Bindings, CastMask, FunctionArgHandle, Match, Matcher};
pub use pat::{IntoPat, Pat};

// ── Capture variable ─────────────────────────────────────────────────────────

pub use var::Capture;

// ── Builder structs ──────────────────────────────────────────────────────────
//
// Re-exported as `pub(crate)` for parity with the rest of the module surface;
// nothing in the crate actually names them via this path (call sites use the
// free functions like `load()` / `phi()`), so the imports are inert.  Kept
// available for in-crate consumers that may want to spell the type explicitly.

#[rustfmt::skip]
#[allow(unused_imports)]
pub(crate) use pat::{
    BoolBinaryOpPat, CallOtherPat, CallPat, FloatBinaryOpPat, FunctionArgPat,
    IfPat, IntBinaryOpPat, LoadPat, MemPhiPat, PhiPat, RetPat, StackStorePat,
    StackStorePhiPat, StorePat, ValuePhiPat,
};

// ── Wildcards, captures, predicates ──────────────────────────────────────────

pub use pat::{any, predicate, var};

// ── Int ops (binary, unary, comparison, variant-agnostic) ────────────────────

#[rustfmt::skip]
pub use pat::{
    add, and, div, int_binary, int_binary_any, int_carry, int_cmp, int_cmp_any,
    int_eq, int_le, int_lt, int_sborrow, int_scarry, int_sle, int_slt,
    bit_not, int_unary, int_unary_any, lzcount, mul, neg, or, popcount,
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
    bool_const, float_const, int_const, int_const_any_of,
    signed_int_const,
};

// ── Memory, phi, function-arg ────────────────────────────────────────────────

#[rustfmt::skip]
pub use pat::{
    function_arg, function_arg_any, function_arg_reg, function_arg_stack,
    load, mem_phi, phi, phi_for, stack_store, stack_store_phi, store, value_phi,
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

pub use strider_ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
