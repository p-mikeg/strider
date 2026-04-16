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
//! - [`Pat`] / [`PatKind`] — pattern values (cheap to clone, reference-counted)
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

mod error;
pub use error::{Error, Result};

mod var;
mod pat;
mod matcher;

// ── Public types ──────────────────────────────────────────────────────────────

pub use var::{Var, NodeVar};
pub use pat::{
    Pat,
    // Builder types
    CallPat, CallOtherPat, RetPat, IfPat, LoadPat, StorePat, StackStorePat, StackStorePhiPat, PhiPat,
    IntBinaryOpPat, BoolBinaryOpPat, FloatBinaryOpPat,
    // Free-function constructors
    any, var, int_const, bool_const, any_const, predicate,
    // Int binary ops
    int_binary, add, sub, mul, div, sdiv, rem, srem,
    and, or, xor, shl, shr, sshr,
    // Int unary ops
    int_unary, neg, not,
    // Int comparisons
    int_cmp, int_eq, int_lt, int_le, int_slt, int_sle,
    int_carry, int_scarry, int_sborrow,
    // Bool ops
    bool_binary, bool_and, bool_or, bool_xor,
    bool_unary, bool_not,
    // Casts / coercions
    cast_to_bool, cast_to_int, truncate, extend, zero_extend, sign_extend, popcount,
    lzcount, piece, extract, insert,
    // Float ops
    float_binary, float_add, float_sub, float_mul, float_div,
    float_unary, float_neg, float_abs, float_sqrt, float_ceil, float_floor, float_round,
    float_cmp, float_eq, float_ne, float_lt, float_le,
    float_is_nan, float_const, any_float_const,
    int_to_float, float_to_int, float_to_float, int_bits_to_float, float_bits_to_int, cast_to_float,
    // Memory
    load, store, stack_store, stack_store_phi,
    // Phi nodes
    phi, phi_for,
    // Entry values
    initial_var, initial_var_for,
    // Control nodes
    call, call_other, ret, if_node,
    // Region search
    contains,
};
pub use matcher::{Matcher, Match, Bindings};

// Re-export op enums so callers can use `int_binary(IntBinaryOp::Add, …)`
// without also depending on the `ir` crate directly.
pub use ir::{IntBinaryOp, IntUnaryOp, IntCmpOp, BoolBinaryOp, BoolUnaryOp, ExtendOp,
             FloatBinaryOp, FloatUnaryOp, FloatCmpOp};
