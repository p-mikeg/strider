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

/// Per-rewrite context for template instantiation.
///
/// Threaded through [`Template::instantiate`](crate::template::Template::instantiate)
/// on every `BuildKind::Fn` evaluation.  Exposes the captured LHS
/// [`Bindings`](crate::Bindings), the matched-root `NodeId` plus its
/// resolved output type, and a borrow on the [`Function`] under
/// rewrite so closures may read side-table state.
pub struct BuildCtx<'a> {
    pub function: &'a Function,
    pub bindings: &'a crate::capture::Bindings,
    pub root: NodeId,
    pub root_ty: NodeOutputType,
}
