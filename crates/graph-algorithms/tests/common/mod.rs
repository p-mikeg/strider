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

pub struct Graph {
    nodes: PrimaryMap<NodeId, Node>,
    nodes_by_name: std::collections::HashMap<String, NodeId>,
}

impl Graph {
    /// The first node named in the DSL input, by construction.
    pub const fn entry(&self) -> NodeId {
        NodeId(0)
    }

    pub fn node(&self, name: &str) -> NodeId {
        self.nodes_by_name[name]
    }

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

/// Each non-blank line is `pred[, pred...] -> succ[, succ...]`, whitespace
/// trimmed.  Names are interned: first appearance creates the node, later
/// ones reuse its id.
///
/// Input is a literal at every call site, so a malformed line panics rather
/// than returning an error.
pub(crate) fn graph(input: &str) -> Graph {
    let mut graph = Graph {
        nodes: PrimaryMap::new(),
        nodes_by_name: std::collections::HashMap::default(),
    };

    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }

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
