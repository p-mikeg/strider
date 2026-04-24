use std::collections::HashMap;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pat::Pat;

mod function_arg_handle;

pub use function_arg_handle::FunctionArgHandle;

mod bindings;
pub(crate) mod commutativity;
mod match_result;
pub(crate) mod walk;

#[cfg(test)]
mod tests;

pub use bindings::Bindings;
pub use match_result::Match;

// ── Matcher ───────────────────────────────────────────────────────────────────

/// Lazy index used by the `FunctionArg` query API ([`Matcher::function_arg`],
/// [`Matcher::function_args`], [`Matcher::function_arg_count`]).
///
/// Built on first access; [`Matcher::match_at`] and [`Matcher::find_all`]
/// never need it.  Layer C of the IR validator enforces at most one
/// `FunctionArg` per index, so at most one entry exists per key.
struct FunctionArgIndex(HashMap<u32, NodeId>);

/// Executes pattern queries against a [`BuiltFunctionGraph`].
///
/// Construction is O(1); the `FunctionArg` index is built lazily on first
/// use of a `function_arg*` query.  `find_all` does a single preorder walk
/// of the graph each call and tries the pattern against every node.
pub struct Matcher<'g> {
    pub(super) fn_graph: &'g BuiltFunctionGraph,
    function_arg_index: std::cell::OnceCell<FunctionArgIndex>,
}

impl<'g> Matcher<'g> {
    /// Creates a new `Matcher`.
    pub fn new(fn_graph: &'g BuiltFunctionGraph) -> Self {
        Self {
            fn_graph,
            function_arg_index: std::cell::OnceCell::new(),
        }
    }

    /// Returns the lazily-built `FunctionArg` index.
    fn function_arg_index(&self) -> &FunctionArgIndex {
        self.function_arg_index.get_or_init(|| {
            let mut map: HashMap<u32, NodeId> = HashMap::new();
            for node in self.fn_graph.preorder() {
                if let NodeKind::FunctionArg { index, .. } =
                    self.fn_graph.graph.node_kind(node)
                {
                    map.insert(*index, node);
                }
            }
            FunctionArgIndex(map)
        })
    }

    /// Finds all nodes in the graph where `pat` matches and returns a [`Match`]
    /// for each.  Does a preorder walk of the graph and tries every node as a
    /// potential root.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        self.fn_graph
            .preorder()
            .filter_map(|node| {
                let mut bindings = Bindings::default();
                if self.match_node_id(node, pat, &mut bindings) {
                    Some(Match {
                        root: node,
                        bindings,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Try to match `pat` against the subgraph rooted at `node`.  Returns the
    /// successful [`Match`] (with bindings) if the match succeeds, `None`
    /// otherwise.
    ///
    /// Unlike [`find_all`] which iterates every candidate root, this checks a
    /// single root.  Used by [`crate::build::rewrite_rule`] and other callers
    /// that already know the candidate.
    pub fn match_at(&self, node: NodeId, pat: &Pat) -> Option<Match> {
        let mut bindings = Bindings::default();
        if self.match_node_id(node, pat, &mut bindings) {
            Some(Match { root: node, bindings })
        } else {
            None
        }
    }

    // ── FunctionArg query API ─────────────────────────────────────────────────

    /// Returns a [`FunctionArgHandle`] for the `FunctionArg` node at argument
    /// position `index`, if the `FunctionArgDetect` pass emitted one.
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'g>> {
        let node_id = *self.function_arg_index().0.get(&index)?;
        self.make_function_arg_handle(node_id)
    }

    /// Returns `max(index) + 1` across all `FunctionArg` nodes in the graph,
    /// or `0` if the graph has none.  Equivalent to "the declared arg count
    /// that `FunctionArgDetect` was able to identify."
    pub fn function_arg_count(&self) -> usize {
        match self.function_arg_index().0.keys().max() {
            Some(&m) => (m as usize) + 1,
            None => 0,
        }
    }

    /// Iterates over every `FunctionArg` node, yielding `(index, handle)`
    /// pairs sorted ascending by index.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'g>)> + '_ {
        let mut pairs: Vec<(u32, NodeId)> = self
            .function_arg_index()
            .0
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        pairs.sort_by_key(|(k, _)| *k);
        pairs.into_iter().filter_map(move |(k, node_id)| {
            self.make_function_arg_handle(node_id).map(|h| (k, h))
        })
    }

    /// Builds a [`FunctionArgHandle`] from `node_id`, pulling `source` and
    /// `index` out of the node's `NodeKind`.  Returns `None` if the node is
    /// not actually a `FunctionArg` — the index-map only contains such nodes
    /// by construction, so this never fires in practice, but preserves the
    /// "no-panic" discipline.
    fn make_function_arg_handle(&self, node_id: NodeId) -> Option<FunctionArgHandle<'g>> {
        let NodeKind::FunctionArg { source, index } = *self.fn_graph.graph.node_kind(node_id)
        else {
            return None;
        };
        Some(FunctionArgHandle {
            fn_graph: self.fn_graph,
            node_id,
            source,
            index,
        })
    }

    // ── Dispatch entry points ────────────────────────────────────────────────
    //
    // `match_output` / `match_node_id` are the single entry points combinators
    // call (via `MatchCtx.matcher`) when recursing into an inner `Pat`.  They
    // forward directly to the pattern's `Pattern::try_match` impl.

    /// Build a [`MatchCtx`](crate::pat::traits::MatchCtx) that carries both
    /// the graph and a back-reference to this matcher.  Combinators clone it
    /// and pass it through their inner [`Self::match_output`] /
    /// [`Self::match_node_id`] dispatch.
    pub(crate) fn ctx(&self) -> crate::pat::traits::MatchCtx<'g, '_> {
        crate::pat::traits::MatchCtx {
            graph: self.fn_graph,
            matcher: self,
        }
    }

    /// Match a `NodeOutputId` against a pattern — single-line delegation to
    /// the unified [`Pattern`](crate::pat::traits::Pattern) trait.
    pub(super) fn match_output(
        &self,
        output: NodeOutputId,
        pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        pat.as_dyn().try_match(&self.ctx(), output, bindings)
    }

    /// Match a `NodeId` against a pattern via
    /// [`Pattern::try_match_node`](crate::pat::traits::Pattern::try_match_node)
    /// — which iterates the node's outputs (default impl) or matches the
    /// node directly (zero-output nodes like `Return`, via `NodePat`'s
    /// override).
    pub(crate) fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        pat.as_dyn().try_match_node(&self.ctx(), node, bindings)
    }
}
