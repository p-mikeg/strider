#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Sea-of-nodes pattern + template crate. Replaces `strider-analyze::pattern`.
//!
//! Internal representation: `PatGraph` backed by `petgraph::StableDiGraph`.
//! Public surface: chained builder free-functions (`add`, `int_const`, `var`,
//! …) plus the `Pattern` and `Template` traits implemented by `PatGraph`.

pub mod bindings;
pub mod builders;
pub mod capture;
pub mod error;
pub mod match_result;
pub mod matcher;
pub mod pat_graph;
pub mod rewrite;
pub mod template;

pub use bindings::{Bindings, BindingsMark};
pub use builders::*;
pub use capture::Capture;
pub use error::{MissingBinding, Result, RewriteSkip, is_skip, missing_binding, skip};
pub use match_result::Match;

// Re-export the IR op enums so callers that consume builder args don't
// need a separate `use strider_ir::IntBinaryOp;` line.  The variant-
// agnostic builders (`int_binary`, `int_cmp`, `float_*`, …) take these
// enums as their first argument; re-exporting them at the crate root
// is the same ergonomic choice strider-analyze's pattern module made.
pub use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
};
pub use matcher::{
    ArgSource, TemplateCtx, CastMask, FunctionArgHandle, MatchCtx, Matcher, MatcherOptions,
    Pattern, PatternExt,
};
pub use pat_graph::{Combine, Concrete, EdgeData, KindSpec, NodeData, PatGraph, Role, Wildcard};
pub use rewrite::{
    BoxedRule, GraphRewriteCtxExt, GraphRewriter, Rewrite, RewriteCtx, RewriteCtxView,
    apply_rules_in_order, boxed_rule, rewrite_rule, rewrite_rule_dynamic,
};
pub use template::Template;

// ── Macros: build-time constants from captures ──────────────────────────────
//
// The `*_const_with!` macros let a rewrite RHS materialise an
// `IntConst` / `FloatConst` (or `I1`-typed bool const) whose value
// depends on LHS-captured operand values.  Each macro expands to a
// call to `{int|bool|float}_const_with_fn` with a closure that
// resolves each named capture against `ctx.bindings` and evaluates
// the body.  See `macros_impl.rs` for the full grammar.
//
// `#[macro_export]` lifts the macros to the crate root so callers
// reach them as `strider_pattern::int_const_with!{…}`.  This module
// hosts the macro bodies; the lift happens at definition time.
mod macros_impl;
