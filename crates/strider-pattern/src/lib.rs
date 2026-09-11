#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

pub mod bindings;
pub mod capture;
pub mod error;
pub(crate) mod graph_ext;
pub mod node_builders;
pub(crate) mod staging;
#[macro_use]
mod macros_impl;
pub mod match_result;
pub mod matcher;
pub mod template;
pub mod typed;

use strider_ir::IRViewer;
use strider_ir::node::ValueKind;

pub use bindings::Bindings;
pub use capture::Capture;
pub use error::{MissingBinding, Result, RewriteSkip, is_skip, missing_binding, skip};
pub use match_result::Match;
pub use matcher::match_pat::{
    CaptureExt, Captured, Guarded, Limited, MatchPat, OfWidth, Ordered, ValueTy,
};
pub use matcher::{
    CastMask, JoinConstraint, JoinPredicateFn, JoinedMatch, Matcher, Pattern, PostMatchFn,
};
pub use node_builders::{
    CallOtherPat, CallPat, EntryPat, FunctionArgClass, FunctionArgPat, IfPat, IndirectBranchPat,
    LoadPat, MemPat, MemPhiPat, OutputPat, PhiPat, RegionPat, RetPat, StorePat, SwitchPat,
    UnreachablePat, WithOutput, any_function_arg, call, call_other, entry, function_arg,
    function_arg_float, function_arg_reg, function_arg_stack, if_else, indirect_branch, load,
    mem_phi, phi, phi_for, region, ret, store, switch, unreachable,
};
pub use template::template_pat::TemplatePat;
pub use template::{Template, TemplateCtx, instantiate};

/// Reached from the `*_const_with!` macros via the reserved `in_ty` identifier.
/// An `IntCmp` root's output type is always `I1`, so signed / carry handling
/// has to read the operand type instead.
pub fn first_value_input_type(ctx: &TemplateCtx<'_>) -> Option<strider_ir::node::ValueType> {
    let inputs = ctx.function.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.function.value_kind(inp) {
        ValueKind::Typed(t) => Some(t),
        _ => None,
    }
}

pub use typed::{
    AltSlot, AnyBool, AnyBoolConst, AnyFloat, AnyFloatConst, AnyInt, AnyIntConst, BoolConstArg,
    BoxedAlt, FloatConstArg, IntConstAnyWidth, IntConstAnyWidthArg, IntConstArg, OneOf, any_bool,
    any_bool_binary, any_bool_const, any_float, any_float_binary, any_float_cmp, any_float_const,
    any_float_unary, any_int, any_int_binary, any_int_cmp, any_int_const, any_int_unary, anything,
    bool_and, bool_binary, bool_const, bool_const_with_fn, bool_inputs, bool_not, bool_or,
    bool_xor, boxed_alt, capture_typed, float_abs, float_add, float_binary, float_bits_to_int,
    float_ceil, float_cmp, float_const, float_const_with_fn, float_div, float_eq, float_floor,
    float_is_nan, float_le, float_lt, float_mul, float_ne, float_neg, float_round, float_sqrt,
    float_sub, float_to_float, float_to_int, initial_var, initial_var_for, inputs_of_width,
    int_add, int_and, int_binary, int_bits_to_float, int_carry, int_cmp, int_const,
    int_const_any_width, int_const_with_fn, int_div, int_eq, int_extend, int_le, int_lt,
    int_lzcount, int_mul, int_ne, int_neg, int_not, int_or, int_popcount, int_rem, int_sborrow,
    int_scarry, int_sdiv, int_shl, int_shr, int_sign_extend, int_sle, int_slt, int_srem, int_sshr,
    int_sub, int_to_float, int_truncate, int_xor, int_zero_extend, predicate, value_of_width, var,
};
