//! Compile-time-typed match-side builder structs + free functions.
//!
//! Each free function returns a typed struct implementing
//! [`MatchPat`](crate::match_pat::MatchPat); structs keep their operand
//! fields as generic type parameters so the template/rewrite side can
//! restate the same shape under a `TemplatePat` bound later. Finalise a
//! built pattern with `.into_pattern()`.

pub mod consts;
pub mod value_ops;
pub mod wildcards;

pub use consts::{
    any_bool_const, any_float_const, any_int_const, bool_const, float_const, int_const,
    int_const_all_ones, int_const_any_of, signed_int_const,
};
pub use value_ops::{
    add, and, bit_not, bool_and, bool_bin_any, bool_binary, bool_not, bool_or, bool_xor, div,
    extend, float_abs, float_add, float_binary, float_binary_any, float_ceil, float_cmp,
    float_cmp_any, float_div, float_eq, float_floor, float_is_nan, float_le, float_lt, float_mul,
    float_ne, float_neg, float_round, float_sqrt, float_sub, float_to_float, float_to_int,
    float_unary_any, int_binary, int_binary_any, int_bits_to_float, int_carry, int_cmp,
    int_cmp_any, int_eq, int_le, int_lt, int_ne, int_sborrow, int_scarry, int_sle, int_slt,
    int_to_float, int_unary_any, lzcount, mul, neg, not_, or, popcount, rem, sdiv, shl, shr,
    sign_extend, srem, sshr, sub, truncate, xor, zero_extend, float_bits_to_int,
};
pub use wildcards::{
    any, bool_inputs, bool_value, initial_var, initial_var_for, inputs_of_width, predicate, value_of_width,
    var,
};
