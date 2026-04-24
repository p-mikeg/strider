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

pub use rewrite::{BoxedRule, RewriteOutcome, apply_rules_in_order, boxed_rule, rewrite_rule};
pub use pat::traits::{BuildCtx, BuildOutcome};
pub use pat::ctor::consts::{FromCtx, first_value_input_type};
pub use pat::ctor::consts::{bool_const_with_fn, float_const_with_fn, int_const_with_fn};

// ── Public types ──────────────────────────────────────────────────────────────

pub use matcher::{Bindings, Match, Matcher};
pub use pat::{
    BoolBinaryOpPat,
    CallOtherPat,
    // Builder types
    CallPat,
    CaptureBuilder,
    FloatBinaryOpPat,
    FunctionArgPat,
    IfPat,
    IntBinaryOpPat,
    // Const-capture overload traits
    IntoAnyBoolConst,
    IntoAnyFloatConst,
    IntoAnyIntConst,
    // Blanket trait
    IntoPat,
    LoadPat,
    MatchPredicateFn,
    Pat,
    PhiPat,
    PredicateFn,
    RetPat,
    StackStorePat,
    StackStorePhiPat,
    StorePat,
    add,
    and,
    // Free-function constructors
    any,
    any_bool_const,
    any_float_const,
    any_int_const,
    bool_and,
    // Bool ops
    bool_binary,
    bool_binary_any,
    bool_const,
    bool_not,
    bool_or,
    bool_unary,
    bool_unary_any,
    bool_xor,
    // Control nodes
    call,
    call_other,
    // Casts / coercions
    cast_to_bool,
    cast_to_float,
    cast_to_int,
    div,
    extend,
    float_abs,
    float_add,
    // Float ops
    float_binary,
    float_binary_any,
    float_bits_to_int,
    float_ceil,
    float_cmp,
    float_cmp_any,
    float_const,
    float_div,
    float_eq,
    float_floor,
    float_le,
    float_lt,
    float_mul,
    float_ne,
    float_neg,
    float_round,
    float_sqrt,
    float_sub,
    float_to_float,
    float_to_int,
    float_unary,
    float_unary_any,
    // Function arguments
    function_arg,
    function_arg_any,
    function_arg_reg,
    function_arg_stack,
    if_node,
    // Entry values
    initial_var,
    initial_var_for,
    // Int binary ops
    int_binary,
    // Variant-agnostic op constructors
    int_binary_any,
    int_bits_to_float,
    int_carry,
    // Int comparisons
    int_cmp,
    int_cmp_any,
    int_const,
    int_eq,
    int_le,
    int_lt,
    int_sborrow,
    int_scarry,
    int_sle,
    int_slt,
    int_to_float,
    // Int unary ops
    int_unary,
    int_unary_any,
    // Memory
    load,
    lzcount,
    mul,
    neg,
    not,
    or,
    // Phi nodes
    phi,
    phi_for,
    popcount,
    predicate,
    rem,
    ret,
    sdiv,
    shl,
    shr,
    sign_extend,
    srem,
    sshr,
    stack_store,
    stack_store_phi,
    store,
    sub,
    truncate,
    var,
    xor,
    zero_extend,
};
pub use var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

// Re-export op enums so callers can use `int_binary(IntBinaryOp::Add, …)`
// without also depending on the `ir` crate directly.
pub use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
