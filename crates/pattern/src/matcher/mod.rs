use std::collections::HashMap;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pat::Pat;

mod function_arg_handle;

pub use function_arg_handle::FunctionArgHandle;

mod bindings;
pub(crate) mod commutativity;
mod match_result;
pub(crate) mod traversal;

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
    /// Top-level `Call`, `CallOther`, `Return`, and `If` patterns use the
    /// pre-indexed node lists (built lazily on first call) and skip the
    /// others — routing is done through
    /// [`crate::pat::traits::ControlPattern::candidate_kind`].
    ///
    /// Data patterns (NodePat) currently have no fast-path routing and fall
    /// through to `all_nodes`. A later optimization could add
    /// `candidate_kind` to [`crate::pat::traits::DataPattern`] to bring back
    /// the `function_arg_nodes` fast path — deferred for now.
    pub fn find_all(&self, pat: &Pat) -> Vec<Match> {
        let idx = self.index();
        let candidates: &[NodeId] = if let Some(ctrl) = pat.as_ctrl() {
            match ctrl.candidate_kind() {
                Some(crate::pat::traits::CandidateKind::Call) => &idx.call_nodes,
                Some(crate::pat::traits::CandidateKind::CallOther) => &idx.call_other_nodes,
                Some(crate::pat::traits::CandidateKind::Return) => &idx.return_nodes,
                Some(crate::pat::traits::CandidateKind::If) => &idx.if_nodes,
                // `FunctionArg` is a data pattern, not a control pattern —
                // reaching this arm would be a bug in a ControlPattern impl.
                // Fall through to `all_nodes` defensively.
                Some(crate::pat::traits::CandidateKind::FunctionArg) | None => &idx.all_nodes,
            }
        } else {
            &idx.all_nodes
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

    // ── Dispatch entry points ────────────────────────────────────────────────
    //
    // `match_output` / `match_node_id` are the single entry points combinators
    // call (via `MatchCtx.matcher`) when recursing into an inner `Pat`.  They
    // forward directly to the pattern's `DataPattern::try_match` /
    // `ControlPattern::try_match` impl.

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

    /// Match a `NodeOutputId` (data edge) against a pattern.  Control
    /// patterns cannot match in a data context and return `false`.
    pub(super) fn match_output(
        &self,
        output: NodeOutputId,
        pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        if let Some(d) = pat.as_dyn() {
            d.try_match(&self.ctx(), output, bindings)
        } else {
            // Ctrl patterns cannot match in a data context.
            false
        }
    }

    /// Match a `NodeId` (control-level node) against a pattern.
    ///
    /// Dispatch:
    /// * `Ctrl(d)` — direct [`crate::pat::traits::ControlPattern::try_match`]
    ///   on the node.
    /// * `Dyn(d)` — a data pattern used as a root candidate: try each output
    ///   of the node against the data pattern.
    pub(crate) fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        let ctx = self.ctx();
        if let Some(c) = pat.as_ctrl() {
            return c.try_match(&ctx, node, bindings);
        }
        // `Pat` has exactly two variants (Dyn and Ctrl); if it isn't Ctrl it
        // must be Dyn.  A data pattern used as a root candidate tries each
        // output of the node against the data pattern.
        let Some(d) = pat.as_dyn() else {
            return false;
        };
        for out in self.fn_graph.graph.node_outputs(node).into_iter() {
            let snap = bindings.clone();
            if d.try_match(&ctx, out, bindings) {
                return true;
            }
            *bindings = snap;
        }
        false
    }
}
