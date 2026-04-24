//! Core trait and type aliases for the pattern engine.
//!
//! A single [`Pattern`] trait targets [`NodeOutputId`] — every pattern,
//! whether "data-level" (arithmetic, loads, phis) or "control-level"
//! (`Call`, `Return`, `If`, `CallOther`), matches against an output.
//! Control patterns internally recover the producing node via
//! `ctx.graph.graph.get_node_from_output(target)` and then do node-level
//! work; any output of the target node is an acceptable match target.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputId};

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

/// The single pattern trait.  Every pattern matches against a
/// [`NodeOutputId`] and fills bindings on success.
pub trait Pattern: Send + Sync {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool;

    /// Node-level match entry.  Default impl iterates the node's outputs
    /// and tries [`try_match`](Self::try_match) on each — the standard
    /// "node as root candidate" semantics used by
    /// [`Matcher::match_node_id`](crate::matcher::Matcher::match_node_id)
    /// and [`Matcher::find_all`](crate::matcher::Matcher::find_all).
    ///
    /// Control patterns override this to match nodes that have no outputs
    /// (e.g. `Return`) — the default "iterate outputs" fails for those.
    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        for out in ctx.graph.graph.node_outputs(node).into_iter() {
            let snap = b.clone();
            if self.try_match(ctx, out, b) {
                return true;
            }
            *b = snap;
        }
        false
    }

    /// Routing hint for [`Matcher::find_all`](crate::matcher::Matcher::find_all).
    /// Returns `Some(kind)` if this pattern is known to match only nodes of
    /// a specific kind (`Call`, `If`, …), enabling the pre-indexed fast
    /// path.  Default is `None` (full-graph scan).
    fn candidate_kind(&self) -> Option<CandidateKind> {
        None
    }
}

/// Reference-counted, erased [`Pattern`] — the single inner type held by
/// every [`crate::pat::Pat`].
pub type DynPat = Arc<dyn Pattern>;

/// Hints [`Matcher::find_all`](crate::matcher::Matcher::find_all) which
/// pre-indexed node list to iterate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Call,
    CallOther,
    Return,
    If,
    /// Reserved for a future `FunctionArg` fast path — no `Pattern` impl
    /// currently returns this variant, so `find_all` falls through to the
    /// full-graph scan.
    #[allow(dead_code)]
    FunctionArg,
}
