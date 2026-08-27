//! Operand fields stay generic type parameters so the template/rewrite side can
//! restate the same shape under a `TemplatePat` bound. Finalise with
//! `.into_pattern()`.

pub mod alternation;
pub mod builder_like;
pub mod consts;
pub mod value_ops;
pub mod wildcards;

pub use alternation::{AltSlot, BoxedAlt, OneOf, boxed_alt};
pub use consts::{
    AnyBoolConst, AnyFloatConst, AnyIntConst, BoolConstArg, FloatConstArg, IntConstAnyWidth,
    IntConstAnyWidthArg, IntConstArg, any_bool_const, any_float_const, any_int_const, bool_const,
    bool_const_with_fn, capture_typed, float_const, float_const_with_fn, int_const,
    int_const_any_width, int_const_with_fn,
};
pub use value_ops::{
    any_bool_binary, any_float_binary, any_float_cmp, any_float_unary, any_int_binary, any_int_cmp,
    any_int_unary, bool_and, bool_binary, bool_not, bool_or, bool_xor, float_abs, float_add,
    float_binary, float_bits_to_int, float_ceil, float_cmp, float_div, float_eq, float_floor,
    float_is_nan, float_le, float_lt, float_mul, float_ne, float_neg, float_round, float_sqrt,
    float_sub, float_to_float, float_to_int, int_add, int_and, int_binary, int_bits_to_float,
    int_carry, int_cmp, int_div, int_eq, int_extend, int_le, int_lt, int_lzcount, int_mul, int_ne,
    int_neg, int_not, int_or, int_popcount, int_rem, int_sborrow, int_scarry, int_sdiv, int_shl,
    int_shr, int_sign_extend, int_sle, int_slt, int_srem, int_sshr, int_sub, int_to_float,
    int_truncate, int_xor, int_zero_extend,
};
pub use wildcards::{
    AnyBool, AnyFloat, AnyInt, any_bool, any_float, any_int, anything, bool_inputs, initial_var,
    initial_var_for, inputs_of_width, predicate, value_of_width, var,
};
