//! Core trait and type aliases for the pattern engine.
//!
//! A single [`Pattern`] trait targets [`NodeOutputId`] — every pattern,
//! whether "data-level" (arithmetic, loads, phis) or "control-level"
//! (`Call`, `Return`, `If`, `CallOther`), matches against an output.
//! Control patterns internally recover the producing node via
//! `ctx.graph.graph.get_node_from_output(target)` and then do node-level
//! work; any output of the target node is an acceptable match target.
//!
//! The trait has a second mode — [`Pattern::try_build`] — for the RHS of
//! [`crate::rewrite_rule`]: materialize this pattern into fresh IR nodes,
//! filling holes from captured bindings. The default impl returns a
//! [`crate::error::NotBuildable`]-wrapped error, so wildcards, guards, and
//! other match-only patterns opt out automatically; buildable patterns
//! override it.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputId, NodeOutputType};

use crate::error::Result;
use crate::matcher::{Bindings, Matcher};

/// Context passed through every [`Pattern::try_match`] call. Carries the
/// graph (for reading node kinds / inputs / outputs / side-tables) and a
/// back-reference to the [`Matcher`] — needed by combinators like
/// `CapturePat`/`WhenPat` that wrap an inner `Pat` and recurse via
/// [`Matcher::match_output`](crate::matcher::Matcher::match_output).
#[derive(Clone, Copy)]
pub struct MatchCtx<'g, 'm> {
    pub graph: &'g BuiltFunctionGraph,
    pub(crate) matcher: &'m Matcher<'g>,
}

impl MatchCtx<'_, '_> {
    /// Value-kind gate: returns `Some(ty)` if `target` is a value output,
    /// `None` for Control / Memory / ControlPhi slots.  Used by `VarPat`,
    /// `CapturePat`, and `GuardPat` because `Capture` bindings refer to data
    /// edges only — on a multi-output node this steers `try_match_node`'s
    /// iteration to the value slot.
    pub(crate) fn require_value_output(
        &self,
        target: NodeOutputId,
    ) -> Option<NodeOutputType> {
        self.graph.graph.output_kind(target).as_value()
    }
}

/// The single pattern trait.  Every pattern matches against a
/// [`NodeOutputId`] and fills bindings on success.  Buildable patterns
/// additionally implement [`Pattern::try_build`] to materialize themselves
/// on the RHS of a rewrite rule.
pub trait Pattern: Send + Sync {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool;

    /// Advertises the kind-level constraint this pattern imposes at its
    /// root.  [`crate::matcher::Matcher::find_all`] uses this to skip
    /// candidate nodes whose kind is incompatible, turning a graph-wide
    /// preorder scan into an effectively kind-indexed scan for patterns
    /// with a concrete root.
    ///
    /// Default: [`crate::pat::node_pat::KindSpec::Any`] (no filtering).
    /// Override on combinators by forwarding to the inner pattern, and on
    /// leaves whose root kind is statically known.
    fn kind_spec(&self) -> crate::pat::node_pat::KindSpec {
        crate::pat::node_pat::KindSpec::Any
    }

    /// Node-level match entry.  Default impl iterates the node's outputs
    /// and tries [`try_match`](Self::try_match) on each — the standard
    /// "node as root candidate" semantics used by
    /// [`Matcher::match_node_id`](crate::matcher::Matcher::match_node_id)
    /// and [`Matcher::find_all`](crate::matcher::Matcher::find_all).
    ///
    /// `NodePat` overrides this to handle zero-output nodes (e.g. `Return`)
    /// — the default "iterate outputs" fails for those.
    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        for out in ctx.graph.graph.node_outputs(node) {
            let mark = b.mark();
            if self.try_match(ctx, out, b) {
                return true;
            }
            b.restore(mark);
        }
        false
    }

    /// Materialize this pattern as fresh IR nodes in `ctx.graph`, using
    /// `ctx.bindings` to fill holes (captures / constant values / operator
    /// variants).  Nodes that inherit the type default to `ctx.root_ty`;
    /// nodes with a fixed result type (cmps, bool ops, bool constants)
    /// override it.
    ///
    /// Default impl returns [`Error::not_buildable`] naming the concrete
    /// pattern type.  Buildable patterns (`NodePat`, `CapturePat`, and any
    /// future build-only leaf) override this.
    fn try_build(&self, _ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        Err(crate::error::not_buildable(std::any::type_name::<Self>()))
    }
}

/// Outcome of a [`Pattern::try_build`] call: either a freshly-created
/// output, or a `Skip` signalling the rewrite doesn't apply after all.
pub enum BuildOutcome {
    Out(NodeOutputId),
    /// Reserved for build-time opt-out; today the rewrite-rule
    /// interpreter detects skips via the [`crate::error::RewriteSkip`]
    /// error sentinel rather than this variant.
    #[allow(dead_code)]
    Skip,
}

/// Mutable context threaded through [`Pattern::try_build`].  Carries the
/// graph being mutated, the match bindings, and the matched root (used
/// by the `int_const_with!` / `bool_const_with!` / `float_const_with!`
/// macros to expose `ty` — the root output type — and `in_ty` — the
/// root's first value input type).
pub struct BuildCtx<'a> {
    pub graph: &'a mut BuiltFunctionGraph,
    pub bindings: &'a Bindings,
    pub root: NodeId,
    pub root_ty: NodeOutputType,
}

/// Reference-counted, erased [`Pattern`] — the single inner type held by
/// every [`crate::pat::Pat`].
pub type DynPat = Arc<dyn Pattern>;

