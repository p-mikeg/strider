#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

use std::ops::ControlFlow;

use cranelift_entity::{PrimaryMap, entity_impl};
use graphwalk::{GraphRef, PredGraphRef};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u32);
entity_impl!(NodeId);

struct Node {
    name: String,
    preds: Vec<NodeId>,
    succs: Vec<NodeId>,
}

/// A small directed graph built from the [`graph`] DSL, used as a fixture
/// for `graphwalk` traversal tests.
///
/// `&Graph` implements [`graphwalk::GraphRef`] and [`graphwalk::PredGraphRef`],
/// so it plugs straight into [`graphwalk::PreOrder`] / [`graphwalk::PostOrder`].
pub struct Graph {
    nodes: PrimaryMap<NodeId, Node>,
    nodes_by_name: std::collections::HashMap<String, NodeId>,
}

impl Graph {
    /// Returns the conventional entry node id (`NodeId(0)`).
    ///
    /// **Precondition:** the input passed to [`graph`] declared at least one
    /// edge — i.e. the graph contains at least one node.  Calling this on an
    /// empty graph returns a stale id that will panic when used as a key into
    /// [`Graph::name`] or any traversal.
    #[must_use]
    pub const fn entry(&self) -> NodeId {
        NodeId(0)
    }

    /// Looks up a node by the name it was given in the DSL.
    ///
    /// # Panics
    ///
    /// Panics if `name` was never declared in the input passed to [`graph`].
    #[must_use]
    pub fn node(&self, name: &str) -> NodeId {
        self.nodes_by_name[name]
    }

    /// Returns the DSL name of `node`.
    ///
    /// # Panics
    ///
    /// Panics if `node` did not originate from this graph.
    #[must_use]
    pub fn name(&self, node: NodeId) -> &str {
        &self.nodes[node].name
    }

    fn get_or_create(&mut self, name: &str) -> NodeId {
        use std::collections::hash_map::Entry;
        match self.nodes_by_name.entry(name.to_owned()) {
            Entry::Occupied(o) => *o.get(),
            Entry::Vacant(v) => {
                let node = self.nodes.push(Node {
                    name: v.key().clone(),
                    preds: Vec::new(),
                    succs: Vec::new(),
                });
                v.insert(node);
                node
            }
        }
    }

    fn add_succ(&mut self, node: NodeId, succ: NodeId) {
        self.nodes[node].succs.push(succ);
        self.nodes[succ].preds.push(node);
    }
}

/// Build a [`Graph`] from a tiny edge-list DSL.
///
/// Each non-blank line has the form `pred[, pred…] -> succ[, succ…]`. Whitespace
/// around names is trimmed. Names are interned: a name's first appearance creates
/// a node, later appearances reuse the same id.
///
/// # Panics
///
/// Panics if a non-blank line does not contain exactly one `->` separator. This
/// helper is test-only; the input is a hard-coded literal in callers, so a
/// malformed line is a programmer error rather than a runtime condition.
#[must_use]
pub fn graph(input: &str) -> Graph {
    let mut graph = Graph {
        nodes: PrimaryMap::new(),
        nodes_by_name: std::collections::HashMap::default(),
    };

    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }

        // graphmock is a test-only DSL helper; input is a hard-coded string in
        // downstream tests, so a malformed line is a programmer error, not a
        // runtime condition that deserves error plumbing.
        #[allow(clippy::panic)]
        let (preds, succs) = line
            .split_once("->")
            .unwrap_or_else(|| panic!("graphmock: line missing `->`: {line:?}"));

        let check_nonempty = |name: &str| {
            assert!(
                !name.is_empty(),
                "graphmock: empty node name in line: {line:?}"
            );
        };

        // `succs` is iterated once per pred, so it must be collected.  `preds`
        // is iterated only once, so we stream it and validate each name inline.
        let succs: Vec<&str> = succs.split(',').map(str::trim).collect();
        for &succ in &succs {
            check_nonempty(succ);
        }

        for pred in preds.split(',').map(str::trim) {
            check_nonempty(pred);
            let pred = graph.get_or_create(pred);
            for &succ in &succs {
                let succ = graph.get_or_create(succ);
                graph.add_succ(pred, succ);
            }
        }
    }

    graph
}

impl GraphRef for &'_ Graph {
    type NodeId = NodeId;

    fn try_successors(
        &self,
        node: Self::NodeId,
        f: impl FnMut(Self::NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.nodes[node].succs.iter().copied().try_for_each(f)
    }
}

impl PredGraphRef for &'_ Graph {
    fn try_predecessors(
        &self,
        node: Self::NodeId,
        f: impl FnMut(Self::NodeId) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        self.nodes[node].preds.iter().copied().try_for_each(f)
    }
}

#[cfg(test)]
mod tests {
    use crate::graph;

    #[test]
    fn simple_graph() {
        let _ = graph(
            "
            a -> b
            b -> c
            c -> d
        ",
        );
    }

    #[test]
    fn diamond() {
        let _ = graph(
            "
            a -> b, c
            b, c -> d
        ",
        );
    }

    #[test]
    fn loop_graph() {
        let _ = graph(
            "
            a -> b
            b -> c
            c -> b
            c -> d
        ",
        );
    }

    use graphwalk::{GraphRef, PredGraphRef};
    use std::ops::ControlFlow;

    fn succs(g: &crate::Graph, node: crate::NodeId) -> Vec<String> {
        let mut out = Vec::new();
        let _ = (&g).try_successors(node, |s| {
            out.push(g.name(s).to_owned());
            ControlFlow::Continue(())
        });
        out
    }

    fn preds(g: &crate::Graph, node: crate::NodeId) -> Vec<String> {
        let mut out = Vec::new();
        let _ = (&g).try_predecessors(node, |p| {
            out.push(g.name(p).to_owned());
            ControlFlow::Continue(())
        });
        out
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn fan_out_and_fan_in() {
        // a, b -> c, d adds 4 edges.
        let g = graph("a, b -> c, d");
        let a = g.node("a");
        let b = g.node("b");
        let c = g.node("c");
        let d = g.node("d");
        assert_eq!(succs(&g, a), vec!["c", "d"]);
        assert_eq!(succs(&g, b), vec!["c", "d"]);
        assert_eq!(preds(&g, c), vec!["a", "b"]);
        assert_eq!(preds(&g, d), vec!["a", "b"]);
    }

    #[test]
    fn self_loop() {
        let g = graph("a -> a");
        let a = g.node("a");
        assert_eq!(succs(&g, a), vec!["a"]);
        assert_eq!(preds(&g, a), vec!["a"]);
    }

    #[test]
    fn name_recurrence_resolves_to_same_id() {
        let g = graph(
            "a -> b
             b -> a",
        );
        let a1 = g.node("a");
        let a2 = g.node("a");
        assert_eq!(a1, a2);
        assert_eq!(succs(&g, a1), vec!["b"]);
        assert_eq!(preds(&g, a1), vec!["b"]);
    }

    #[test]
    #[should_panic(expected = "graphmock: empty node name")]
    fn empty_succ_token_panics() {
        // "a -> " trims to ("a", ""): the empty successor used to silently
        // create a phantom node.  Reject as malformed.
        let _ = graph("a -> ");
    }

    #[test]
    #[should_panic(expected = "graphmock: empty node name")]
    fn empty_pred_token_panics() {
        let _ = graph(" -> b");
    }

    #[test]
    #[should_panic(expected = "graphmock: empty node name")]
    fn trailing_comma_panics() {
        let _ = graph("a, -> b");
    }
}
