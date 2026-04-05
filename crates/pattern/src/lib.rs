mod var;
mod pat;
mod matcher;

// ── Public types ──────────────────────────────────────────────────────────────

pub use var::{Var, NodeVar};
pub use pat::{
    Pat,
    // Builder types
    CallPat, RetPat, IfPat, LoadPat, StorePat, SelectorPat,
    // Free-function constructors
    any, var, int_const, bool_const,
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
    // Memory
    load, store,
    // Selector / phi
    selector, selector_for,
    // Entry values
    initial_var, initial_var_for,
    // Control nodes
    call, ret, if_node,
    // Region search
    contains,
};
pub use matcher::{Matcher, Match, Bindings};

// Re-export op enums so callers can use `int_binary(IntBinaryOp::Add, …)`
// without also depending on the `ir` crate directly.
pub use ir::{IntBinaryOp, IntUnaryOp, IntCmpOp, BoolBinaryOp, BoolUnaryOp, ExtendOp};
