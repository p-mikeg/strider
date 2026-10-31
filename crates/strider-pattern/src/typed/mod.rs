//! One set of typed factories for both sides: an operand stays a generic type
//! parameter, so a value op is a `MatchPat` when its operands are and a
//! `TemplatePat` when they are. Finalise with `.into_pattern()` or
//! `.into_template()`.

pub(crate) mod alternation;
pub(crate) mod consts;
pub(crate) mod value_ops;
pub(crate) mod wildcards;

pub use alternation::{BoxedAlt, OneOf, boxed_alt};
pub use consts::{
    BoolConstArg, FloatConstArg, IntConstAnyWidthArg, IntConstArg, any_bool_const, any_float_const,
    any_int_const, bool_const, bool_const_with_fn, capture_typed, float_const, float_const_with_fn,
    int_const, int_const_all_ones, int_const_any_width, int_const_with_fn,
};
pub use value_ops::{
    MatchOnly, Op, any_bool_binary, any_float_binary, any_float_cmp, any_float_unary,
    any_int_binary, any_int_cmp, any_int_unary, bool_and, bool_binary, bool_not, bool_or, bool_xor,
    float_abs, float_add, float_binary, float_bits_to_int, float_ceil, float_cmp, float_div,
    float_eq, float_floor, float_is_nan, float_le, float_lt, float_mul, float_ne, float_neg,
    float_round, float_sqrt, float_sub, float_to_float, float_to_int, float_unary, int_add,
    int_and, int_binary, int_bits_to_float, int_carry, int_cmp, int_div, int_eq, int_extend,
    int_le, int_lt, int_lzcount, int_mul, int_ne, int_neg, int_not, int_or, int_popcount, int_rem,
    int_sborrow, int_scarry, int_sdiv, int_shl, int_shr, int_sign_extend, int_sle, int_slt,
    int_srem, int_sshr, int_sub, int_to_float, int_truncate, int_xor, int_zero_extend,
};
pub use wildcards::{
    any_bool, any_float, any_int, anything, bool_inputs, initial_var, initial_var_for,
    inputs_of_width, predicate, value_of_width, var,
};
