//! Test-only graph DSL — moved inline from the standalone `graphmock`
//! crate (which had no production consumers beyond these tests).
//!
//! `&Graph` implements [`graph_algorithms::walk::GraphRef`], so it plugs straight into
//! [`graph_algorithms::walk::PreOrder`] / [`graph_algorithms::walk::PostOrder`].

#![allow(
    dead_code,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use std::ops::ControlFlow;

use cranelift_entity::{PrimaryMap, entity_impl};
use graph_algorithms::walk::GraphRef;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(u32);
entity_impl!(NodeId);

struct Node {
    name: String,
    succs: Vec<NodeId>,
}

/// A small directed graph built from the [`graph`] DSL, used as a fixture
/// for `graph-algorithms` traversal tests.
pub struct Graph {
    nodes: PrimaryMap<NodeId, Node>,
    nodes_by_name: std::collections::HashMap<String, NodeId>,
}

impl Graph {
    /// Returns the conventional entry node id (`NodeId(0)`).
    pub const fn entry(&self) -> NodeId {
        NodeId(0)
    }

    /// Looks up a node by the name it was given in the DSL.
    pub fn node(&self, name: &str) -> NodeId {
        self.nodes_by_name[name]
    }

    /// Returns the DSL name of `node`.
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
                    succs: Vec::new(),
                });
                v.insert(node);
                node
            }
        }
    }

    fn add_succ(&mut self, node: NodeId, succ: NodeId) {
        self.nodes[node].succs.push(succ);
    }
}

/// Build a [`Graph`] from a tiny edge-list DSL.
///
/// Each non-blank line has the form `pred[, pred…] -> succ[, succ…]`.
/// Whitespace around names is trimmed.  Names are interned: a name's
/// first appearance creates a node, later appearances reuse the same id.
///
/// Test-only helper — input is a hard-coded literal in callers, so a
/// malformed line panics rather than returning an error.
pub(crate) fn graph(input: &str) -> Graph {
    let mut graph = Graph {
        nodes: PrimaryMap::new(),
        nodes_by_name: std::collections::HashMap::default(),
    };

    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }

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
