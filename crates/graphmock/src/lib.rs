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

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);
entity_impl!(NodeId);

struct Node {
    name: String,
    preds: Vec<NodeId>,
    succs: Vec<NodeId>,
}

pub struct Graph {
    nodes: PrimaryMap<NodeId, Node>,
    nodes_by_name: std::collections::HashMap<String, NodeId>,
}

impl Graph {
    pub fn entry(&self) -> NodeId {
        NodeId(0)
    }

    pub fn node(&self, name: &str) -> NodeId {
        self.nodes_by_name[name]
    }

    pub fn name(&self, node: NodeId) -> &str {
        &self.nodes[node].name
    }

    fn get_or_create(&mut self, name: &str) -> NodeId {
        if let Some(&node) = self.nodes_by_name.get(name) {
            return node;
        }

        let node = self.nodes.push(Node {
            name: name.to_owned(),
            preds: Vec::new(),
            succs: Vec::new(),
        });
        self.nodes_by_name.insert(name.to_owned(), node);
        node
    }

    fn add_succ(&mut self, node: NodeId, succ: NodeId) {
        self.nodes[node].succs.push(succ);
        self.nodes[succ].preds.push(node);
    }
}

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
        #[allow(clippy::unwrap_used)]
        let [preds, succs]: [&str; 2] = line.split("->").collect::<Vec<_>>().try_into().unwrap();
        let preds = preds.split(',').map(|pred| pred.trim());
        let succs: Vec<_> = succs.split(',').map(|succ| succ.trim()).collect();

        for pred in preds {
            let pred = graph.get_or_create(pred);
            for succ in &succs {
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
        graph(
            "
            a -> b
            b -> c
            c -> d
        ",
        );
    }

    #[test]
    fn diamond() {
        graph(
            "
            a -> b, c
            b, c -> d
        ",
        );
    }

    #[test]
    fn loop_grpah() {
        graph(
            "
            a -> b
            b -> c
            c -> b
            c -> d
        ",
        );
    }
}
