#![allow(
    dead_code,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

pub mod cases;

use std::cell::Cell;

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
    /// Calls to `successors`, the unit the scale tests count.
    succ_calls: Cell<u64>,
    /// The call that would exceed this panics.
    succ_budget: Cell<u64>,
}

impl Graph {
    fn empty() -> Self {
        Self {
            nodes: PrimaryMap::new(),
            nodes_by_name: std::collections::HashMap::default(),
            succ_calls: Cell::new(0),
            succ_budget: Cell::new(u64::MAX),
        }
    }

    /// Nodes `0..n`, unnamed, successors in `edges` order.
    pub fn from_edges(n: u32, edges: &[(u32, u32)]) -> Self {
        let mut graph = Self::empty();
        for _ in 0..n {
            graph.nodes.push(Node {
                name: String::new(),
                succs: Vec::new(),
            });
        }
        for &(a, b) in edges {
            graph.add_succ(NodeId(a), NodeId(b));
        }
        graph
    }

    pub fn succ_calls(&self) -> u64 {
        self.succ_calls.get()
    }

    /// Fails a superlinear walk at the first call over `calls`, before it
    /// can exhaust time or memory.
    pub fn set_succ_budget(&self, calls: u64) {
        self.succ_budget.set(calls);
    }

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
    let mut graph = Graph::empty();

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

    fn successors(&self, node: Self::NodeId, mut f: impl FnMut(Self::NodeId)) {
        let calls = self.succ_calls.get() + 1;
        assert!(
            calls <= self.succ_budget.get(),
            "{calls} successor calls exceed the budget"
        );
        self.succ_calls.set(calls);
        for succ in self.nodes[node].succs.iter().copied() {
            f(succ);
        }
    }
}
