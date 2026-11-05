pub(crate) mod bindings;
pub(crate) mod build_graph;
pub(crate) mod capture;
pub(crate) mod error;
pub(crate) mod expr;
pub(crate) mod graph_ext;
pub mod node_builders;
#[macro_use]
mod macros_impl;
pub(crate) mod match_result;
pub mod matcher;
pub mod template;
pub(crate) mod typed;

use strider_ir::IRViewer;

pub use bindings::Bindings;
pub use capture::Capture;
pub use error::{OperandWidthMismatch, Result, RewriteSkip, is_skip, missing_binding, skip};
pub use expr::OutputAspect;
#[doc(hidden)]
pub use expr::{Expr, IntoExpr};
pub use match_result::Match;
pub use matcher::match_pat::{CaptureExt, Captured, MatchPat};
pub use matcher::{
    CastMask, JoinConstraint, JoinPredicateFn, JoinedMatch, MAX_PATTERN_NODES, Matcher, Pattern,
    PostMatchFn,
};
pub use node_builders::{
    CallOtherPat, CallPat, EntryPat, FunctionArgClass, FunctionArgPat, IfPat, IndirectBranchPat,
    LoadPat, MemPat, MemPhiPat, OutputPat, PhiPat, RegionPat, RetPat, StorePat, SwitchPat,
    UnreachablePat, WithOutput, any_function_arg, call, call_other, entry, function_arg,
    function_arg_float, function_arg_reg, function_arg_stack, if_else, indirect_branch, load,
    mem_phi, phi, phi_for, region, ret, store, switch, unreachable,
};
pub use template::template_pat::TemplatePat;
pub use template::{Template, TemplateCtx, instantiate, instantiate_guarded};

/// The root's first operand type, `in_ty` in the `*_const_with!` macros (an `IntCmp` root's own
/// type is `I1`).
pub fn first_value_input_type(ctx: &TemplateCtx<'_>) -> Option<strider_ir::node::ValueType> {
    let input = ctx.function.node_inputs(ctx.root).into_iter().next()?;
    ctx.function.value_kind(input).as_value()
}

pub use typed::*;
