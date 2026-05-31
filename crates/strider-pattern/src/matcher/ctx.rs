// `dead_code` allow: `BuildCtx` is wired in a subsequent task (template
// instantiation).  `MatchCtx::matcher` is read by the upcoming PatGraph
// `Pattern` impl.  Module-level allow keeps the `-D warnings` build green
// until those call sites land.
#![allow(dead_code)]

//! Context structs threaded through matching + template instantiation.

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeOutputType};

use super::Matcher;

/// Per-attempt context for matching.  Carries the function under
/// inspection and a reference to the owning matcher.  `Copy` because
/// both fields are references.
#[derive(Clone, Copy)]
pub struct MatchCtx<'a> {
    pub matcher: &'a Matcher<'a>,
    pub function: &'a Function,
}

/// Per-rewrite context for template instantiation (wired in subsequent task).
pub struct BuildCtx<'a> {
    pub function: &'a mut Function,
    pub bindings: &'a crate::capture::Bindings,
    pub root: NodeId,
    pub root_ty: NodeOutputType,
}
