use std::collections::HashMap;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pat::{Pat, PatKind};

mod function_arg_handle;

pub use function_arg_handle::FunctionArgHandle;

mod bindings;
pub(crate) mod commutativity;
mod control;
mod match_result;
mod traversal;

#[cfg(test)]
mod tests;

pub use bindings::Bindings;
pub use match_result::Match;

// ── Matcher ───────────────────────────────────────────────────────────────────

/// Precomputed per-kind `NodeId` lists used by [`Matcher::find_all`] to skip
/// the full-graph scan when the pattern root is `Call`, `CallOther`, `Return`,
/// or `If`.
///
/// Built lazily on the first `find_all` call; `match_at` never needs it.
struct NodeIndex {
    call_nodes: Vec<NodeId>,
    call_other_nodes: Vec<NodeId>,
    return_nodes: Vec<NodeId>,
    if_nodes: Vec<NodeId>,
    /// `NodeId` of the canonical `FunctionArg` for each argument index.  Layer
    /// C enforces at most one `FunctionArg` per index, so at most one entry
    /// exists per key.
    function_args: HashMap<u32, NodeId>,
    all_nodes: Vec<NodeId>,
}

/// Executes pattern queries against a [`BuiltFunctionGraph`].
///
/// Construction is O(1): the per-kind node indices used by
/// [`Matcher::find_all`] are populated lazily on first use.  Consumers that
/// only call [`Matcher::match_at`] (e.g. `rewrite_rule`) never pay the
/// indexing cost.
pub struct Matcher<'g> {
    pub(super) fn_graph: &'g BuiltFunctionGraph,
    index: std::cell::OnceCell<NodeIndex>,
}

impl<'g> Matcher<'g> {
    /// Creates a new `Matcher`.  O(1); index construction is deferred until
    /// the first [`Matcher::find_all`] call.
    pub fn new(fn_graph: &'g BuiltFunctionGraph) -> Self {
        Self {
            fn_graph,
            index: std::cell::OnceCell::new(),
        }
    }

    /// Returns the lazily-built node index, constructing it on first access.
    fn index(&self) -> &NodeIndex {
        self.index.get_or_init(|| {
            let mut call_nodes = Vec::new();
            let mut call_other_nodes = Vec::new();
            let mut return_nodes = Vec::new();
            let mut if_nodes = Vec::new();
            let mut function_args: HashMap<u32, NodeId> = HashMap::new();
            let mut all_nodes = Vec::new();

            for node in self.fn_graph.preorder() {
                all_nodes.push(node);
                match self.fn_graph.graph.node_kind(node) {
                    NodeKind::Call => call_nodes.push(node),
                    NodeKind::CallOther { .. } => call_other_nodes.push(node),
                    NodeKind::Return => return_nodes.push(node),
                    NodeKind::If => if_nodes.push(node),
                    NodeKind::FunctionArg { index, .. } => {
                        function_args.insert(*index, node);
                    }
                    _ => {}
                }
            }

            NodeIndex {
                call_nodes,
                call_other_nodes,
                return_nodes,
                if_nodes,
                function_args,
                all_nodes,
            }
        })
    }

    /// Finds all nodes in the graph where `pat` matches and returns a [`Match`]
    /// for each.
    ///
    /// The search is exhaustive: every node is tried as a potential root.
    /// Top-level `Call`, `Return`, and `If` patterns use the pre-indexed node
    /// lists (built lazily on first call) and skip the others.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        let idx = self.index();
        // Control-level candidate routing still peeks the legacy `PatKind`
        // when the pat is on the legacy path.  When the pat migrates to
        // `Dyn`, routing falls back to `all_nodes` (Phase 3 will replace this
        // with `ControlPattern::candidate_kind`).
        let candidates: &[NodeId] = match pat.as_legacy() {
            Some(PatKind::Call { .. }) => &idx.call_nodes,
            Some(PatKind::CallOther { .. }) => &idx.call_other_nodes,
            Some(PatKind::Return { .. }) => &idx.return_nodes,
            Some(PatKind::If { .. }) => &idx.if_nodes,
            // FunctionArg migrated to the trait-based engine in Phase 2.7 —
            // it no longer has a `PatKind` variant, so the pat is `Dyn` and
            // falls through to `all_nodes`.  Phase 3 will restore the
            // fast-path via `ControlPattern::candidate_kind`.
            _ => &idx.all_nodes,
        };

        candidates
            .iter()
            .filter_map(|&node| {
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
        let idx = self.index();
        let node_id = *idx.function_args.get(&index)?;
        self.make_function_arg_handle(node_id)
    }

    /// Returns `max(index) + 1` across all `FunctionArg` nodes in the graph,
    /// or `0` if the graph has none.  Equivalent to "the declared arg count
    /// that `FunctionArgDetect` was able to identify."
    pub fn function_arg_count(&self) -> usize {
        let idx = self.index();
        match idx.function_args.keys().max() {
            Some(&m) => (m as usize) + 1,
            None => 0,
        }
    }

    /// Iterates over every `FunctionArg` node, yielding `(index, handle)`
    /// pairs sorted ascending by index.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'g>)> + '_ {
        let idx = self.index();
        let mut pairs: Vec<(u32, NodeId)> =
            idx.function_args.iter().map(|(&k, &v)| (k, v)).collect();
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

    // ── delegating shells ─────────────────────────────────────────────────────
    //
    // The real per-family dispatch lives in `data/` and `control.rs`; these
    // shells keep the `&self.match_output(...)` / `&self.match_node_id(...)`
    // call-sites stable for submodule callers that already hold a `&Matcher`.

    /// Build a [`MatchCtx`](crate::pat::traits::MatchCtx) that carries both
    /// the graph and a back-reference to this matcher.  Combinators call this
    /// to dispatch through [`Self::match_output`] / [`Self::match_node_id`]
    /// when their inner pattern might still be on the transitional Legacy
    /// path.
    pub(crate) fn ctx(&self) -> crate::pat::traits::MatchCtx<'g, '_> {
        crate::pat::traits::MatchCtx {
            graph: self.fn_graph,
            matcher: self,
        }
    }

    /// Match a `NodeOutputId` (data edge) against a pattern.  All data-level
    /// pattern kinds have migrated to the trait-based engine; the remaining
    /// `Legacy` variants are control-level (`Call` / `CallOther` / `Return`
    /// / `If` / `Contains`) which cannot match in a data context and return
    /// `false` here.  Phase 3 will migrate those, after which the `Legacy`
    /// branch goes away entirely.
    pub(super) fn match_output(
        &self,
        output: NodeOutputId,
        pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        if let Some(d) = pat.as_dyn() {
            let ctx = self.ctx();
            d.try_match(&ctx, output, bindings)
        } else {
            // Legacy variants here are all control-level; they never match
            // against a data output.
            false
        }
    }

    /// Match a `NodeId` (control-level node) against a pattern.  Legacy
    /// `PatKind`-backed pats route through the existing control dispatcher.
    /// Trait-backed data pats mirror the legacy fallthrough: try each output
    /// of `node` against the data pattern.  (Phase 3 will add a dedicated
    /// `ControlPattern` path here.)
    pub(super) fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        if pat.as_legacy().is_some() {
            control::match_node_id(self, node, pat, bindings)
        } else if let Some(d) = pat.as_dyn() {
            let ctx = self.ctx();
            for out in self.fn_graph.graph.node_outputs(node).into_iter() {
                let snap = bindings.clone();
                if d.try_match(&ctx, out, bindings) {
                    return true;
                }
                *bindings = snap;
            }
            false
        } else {
            false
        }
    }
}
