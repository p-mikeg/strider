#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Sea-of-nodes pattern + template crate.
//!
//! Internal representation: [`pattern::Pattern`] is backed by the generic
//! [`bigraph::BiGraph<N, O>`], which mirrors the IR's `Node → ValueData →
//! Node` structure with two vertex kinds (node / output) and two edge kinds
//! (`Produces` / `Consumes`). `Pattern` instantiates it as
//! `BiGraph<PatNode, PatValue>`; the `petgraph` backing is an
//! implementation detail private to the [`bigraph`] module.

pub mod bigraph;
pub mod bindings;
pub mod builder;
pub mod capture;
pub mod control;
pub mod error;
#[macro_use]
mod macros_impl;
pub mod match_pat;
pub mod match_result;
pub mod matcher;
pub mod pattern;
pub mod rewrite;
pub mod template;
pub mod template_pat;
pub mod typed;

pub use bindings::Bindings;
pub use capture::Capture;
pub use control::{
    CallOtherPat, CallPat, FunctionArgPat, IfPat, LoadPat, MemPat, MemPhiPat, PhiPat, RetPat,
    StorePat, call, call_other, function_arg, function_arg_any, function_arg_reg,
    function_arg_stack, if_node, load, mem_phi, phi, phi_for, ret, store,
};
pub use error::{MissingBinding, Result, RewriteSkip, is_skip, missing_binding, skip};
pub use match_pat::{
    CaptureExt, Captured, Guarded, Limited, MatchPat, OfWidth, Ordered, ValueTy,
};
pub use match_result::Match;
pub use matcher::{CastMask, Matcher};
pub use pattern::{Pattern, PostMatchFn};
pub use rewrite::{
    BoxedRule, GraphRewriteCtxExt, GraphRewriter, RewriteCtx, RewriteCtxView, apply_rules_in_order,
    boxed_rule, rewrite_rule, rewrite_rule_runtime,
};
pub use template::{Template, TemplateCtx, instantiate};
pub use template_pat::TemplatePat;

/// Returns the [`ValueType`](strider_ir::node::ValueType) of
/// the matched root's first value input, or `None` if the root has no
/// inputs or its first input isn't a value edge.
///
/// Exposed for the `*_const_with!` macros via the magic `in_ty`
/// identifier — for `IntCmp(lhs, rhs)` rules where the comparison's
/// input type (needed for signed / carry handling) differs from the
/// root's output type (always `I1`).
#[must_use]
pub fn first_value_input_type(
    ctx: &TemplateCtx<'_>,
) -> Option<strider_ir::node::ValueType> {
    use strider_ir::node::ValueKind;
    let inputs = ctx.function.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.function.value_kind(inp) {
        ValueKind::Typed(t) => Some(t),
        _ => None,
    }
}

pub use typed::{
    add, and, any, any_bool_const, any_float_const, any_int_const, bit_not, bool_and,
    bool_bin_any, bool_binary, bool_const, bool_const_with_fn, bool_inputs, bool_not, bool_or,
    bool_value, bool_xor, float_const_with_fn, int_const_with_fn,
    div, extend, float_abs, float_add, float_binary, float_binary_any, float_bits_to_int,
    float_ceil, float_cmp, float_cmp_any, float_const, float_div, float_eq, float_floor,
    float_is_nan, float_le, float_lt, float_mul, float_ne, float_neg, float_round, float_sqrt,
    float_sub, float_to_float, float_to_int, float_unary_any, initial_var, initial_var_for,
    inputs_of_width, int_binary, int_binary_any, int_bits_to_float, int_carry, int_cmp,
    int_cmp_any, int_const, int_const_any_of, int_eq, int_le, int_lt, int_ne,
    int_sborrow, int_scarry, int_sle, int_slt, int_to_float, int_unary_any, lzcount, mul, neg,
    not_, or, popcount, predicate, rem, sdiv, shl, shr, sign_extend, signed_int_const, srem, sshr,
    sub, truncate, value_of_width, var, xor, zero_extend,
};
