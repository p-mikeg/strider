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
//! Internal representation: a bipartite [`pattern::Pattern`] backed by
//! `petgraph::StableDiGraph`, mirroring the IR's `Node → NodeOutput →
//! Node` structure with real `PatNode` and `PatOutput` vertices.

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
    LoadPat, MemPat, MemPhiPat, PhiPat, StorePat, ValuePhiPat, load, mem_phi, phi, phi_for, store,
    value_phi,
};
pub use error::{MissingBinding, Result, RewriteSkip, is_skip, missing_binding, skip};
pub use match_pat::{CaptureExt, Captured, Guarded, Limited, MatchPat, Ordered};
pub use match_result::Match;
pub use matcher::Matcher;
pub use rewrite::{
    BoxedRule, GraphRewriteCtxExt, GraphRewriter, RewriteCtx, RewriteCtxView, apply_rules_in_order,
    assert_buildable, boxed_rule, rewrite_rule, rewrite_rule_runtime,
};
pub use template::{TemplateCtx, instantiate};
pub use template_pat::TemplatePat;

/// Returns the [`NodeOutputType`](strider_ir::node::NodeOutputType) of
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
) -> Option<strider_ir::node::NodeOutputType> {
    use strider_ir::node::NodeOutputKind;
    let inputs = ctx.function.node_inputs(ctx.root);
    let inp = inputs.into_iter().next()?;
    match ctx.function.output_kind(inp) {
        NodeOutputKind::OutputType(t) => Some(t),
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
    int_cmp_any, int_const, int_const_all_ones, int_const_any_of, int_eq, int_le, int_lt, int_ne,
    int_sborrow, int_scarry, int_sle, int_slt, int_to_float, int_unary_any, lzcount, mul, neg,
    not_, or, popcount, predicate, rem, sdiv, shl, shr, sign_extend, signed_int_const, srem, sshr,
    sub, truncate, value_of_width, var, xor, zero_extend,
};
