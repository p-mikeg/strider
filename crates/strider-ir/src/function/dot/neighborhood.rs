//! Structural neighborhood renderer for the interactive explorer.
//!
//! Unlike the pretty [`super::FunctionDotDumper`] (which inlines constants and
//! adds virtual If-branch / Post-Call nodes, so it is neither 1:1 nor
//! bijective with the IR), this renders the graph **structurally**: exactly one
//! DOT node per IR node and one edge per IR input edge, so a DOT node id *is*
//! an IR `NodeId`. That bijection is what lets the explorer map pattern-match
//! results onto shown nodes and compute neighborhoods that match what's drawn.
//! It reuses the pretty styling ([`super::node_shape`] / [`super::node_fillcolor`])
//! and labels ([`FunctionDotDumper::pretty_label`]).

use std::collections::VecDeque;
use std::io;

use rsleigh::MemReader;
use rustc_hash::{FxHashMap, FxHashSet};

use super::{FunctionDotDumper, node_fillcolor, node_shape};
use crate::function::Function;
use crate::node::NodeId;
use crate::{IRViewer, IRWalker};

/// Maps each node to the nodes that consume one of its outputs (the forward
/// edges the IR doesn't index directly). Built by walking every reachable
/// node's inputs once — `O(V+E)`.
pub(super) fn build_consumers(f: &Function) -> FxHashMap<NodeId, Vec<NodeId>> {
    let mut consumers: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    for node in f.walk() {
        for value in f.node_inputs(node) {
            let (producer, _slot) = f.value_definition(value);
            consumers.entry(producer).or_default().push(node);
        }
    }
    consumers
}

/// The producer node feeding each of `node`'s inputs.
fn producers(f: &Function, node: NodeId) -> Vec<NodeId> {
    f.node_inputs(node)
        .into_iter()
        .map(|v| f.value_definition(v).0)
        .collect()
}

/// BFS the depth-`depth` neighborhood around `center` over **both** input and
/// output edges. A node whose total degree exceeds `hub_cap` is included but
/// not expanded *through* (a value like the memory token or a constant used in
/// hundreds of places would otherwise pull the whole function in at hop 1);
/// `center` always expands. Returns the set of nodes to draw.
pub(super) fn neighborhood_nodes(
    f: &Function,
    center: NodeId,
    depth: usize,
    hub_cap: usize,
    consumers: &FxHashMap<NodeId, Vec<NodeId>>,
) -> FxHashSet<NodeId> {
    let mut seen = FxHashSet::default();
    seen.insert(center);
    let mut queue = VecDeque::from([(center, 0usize)]);
    while let Some((node, dist)) = queue.pop_front() {
        if dist >= depth {
            continue;
        }
        let prod = producers(f, node);
        let cons = consumers.get(&node).cloned().unwrap_or_default();
        if node != center && prod.len() + cons.len() > hub_cap {
            continue; // don't expand through a hub
        }
        for nb in prod.into_iter().chain(cons) {
            if seen.insert(nb) {
                queue.push_back((nb, dist + 1));
            }
        }
    }
    seen
}

impl<R: MemReader> FunctionDotDumper<'_, R> {
    /// Renders the structural depth-`depth` neighborhood around `center` to a
    /// standalone DOT string (see the module docs). The `center` node is
    /// highlighted with a bright border. DOT node ids are IR node ids.
    ///
    /// # Errors
    /// Propagates a `pretty_label` IO error (e.g. a Sleigh register-name
    /// lookup failure).
    pub fn neighborhood_dot(
        &self,
        center: NodeId,
        depth: usize,
        hub_cap: usize,
    ) -> io::Result<String> {
        let consumers = build_consumers(self.function);
        let set = neighborhood_nodes(self.function, center, depth, hub_cap, &consumers);

        let mut out = ::dot::DotEmitter::new("G", &::dot::DotStyle::dark());
        for &node in &set {
            let id = node.as_u32().to_string();
            let kind = self.function.node_kind(node);
            let label = self.pretty_label(node)?;
            let mut extra: Vec<(&str, &str)> = vec![("fillcolor", node_fillcolor(kind))];
            if node == center {
                extra.push(("color", "\"#ffcc00\""));
                extra.push(("penwidth", "2.5"));
            }
            out.node(&id, &label, node_shape(kind), &extra);
        }
        // One edge per IR input edge whose producer is also in the set.
        for &node in &set {
            let id = node.as_u32().to_string();
            for value in self.function.node_inputs(node) {
                let (producer, _) = self.function.value_definition(value);
                if set.contains(&producer) {
                    out.edge(&producer.as_u32().to_string(), &id, &[]);
                }
            }
        }
        Ok(out.finish())
    }
}
