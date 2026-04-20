use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::pat::{Pat, PatKind};

mod bindings;
mod commutativity;
mod control;
mod data;
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
            let mut all_nodes = Vec::new();

            for node in self.fn_graph.preorder() {
                all_nodes.push(node);
                match self.fn_graph.graph.node_kind(node) {
                    NodeKind::Call => call_nodes.push(node),
                    NodeKind::CallOther { .. } => call_other_nodes.push(node),
                    NodeKind::Return => return_nodes.push(node),
                    NodeKind::If => if_nodes.push(node),
                    _ => {}
                }
            }

            NodeIndex {
                call_nodes,
                call_other_nodes,
                return_nodes,
                if_nodes,
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
        let candidates: &[NodeId] = match pat.inner() {
            PatKind::Call { .. } => &idx.call_nodes,
            PatKind::CallOther { .. } => &idx.call_other_nodes,
            PatKind::Return { .. } => &idx.return_nodes,
            PatKind::If { .. } => &idx.if_nodes,
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

    // ── delegating shells ─────────────────────────────────────────────────────
    //
    // The real per-family dispatch lives in `data/` and `control.rs`; these
    // shells keep the `&self.match_output(...)` / `&self.match_node_id(...)`
    // call-sites stable for submodule callers that already hold a `&Matcher`.

    /// Match a `NodeOutputId` (data edge) against a pattern.  Delegates to
    /// [`data::match_output`].
    pub(super) fn match_output(
        &self,
        output: NodeOutputId,
        pat: &Pat,
        bindings: &mut Bindings,
    ) -> bool {
        data::match_output(self, output, pat, bindings)
    }

    /// Match a `NodeId` (control-level node) against a pattern.  Delegates to
    /// [`control::match_node_id`].
    pub(super) fn match_node_id(&self, node: NodeId, pat: &Pat, bindings: &mut Bindings) -> bool {
        control::match_node_id(self, node, pat, bindings)
    }
}
