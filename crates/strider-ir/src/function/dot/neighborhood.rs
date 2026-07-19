//! Neighborhood selection for the interactive explorer.  Only the node SET is
//! computed here; the render is the ordinary pretty dumper restricted to it.

use std::collections::VecDeque;
use std::io;

use rsleigh::MemReader;
use rustc_hash::{FxHashMap, FxHashSet};

use super::FunctionDotDumper;
use crate::function::Function;
use crate::node::NodeId;
use crate::{IRViewer, IRWalker};

/// Forward edges, which the IR doesn't index directly.
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

fn producers(f: &Function, node: NodeId) -> Vec<NodeId> {
    f.node_inputs(node)
        .into_iter()
        .map(|v| f.value_definition(v).0)
        .collect()
}

/// BFS around `center` over **both** input and output edges, capped at
/// `max_nodes`.  A node whose total degree exceeds `hub_cap` is included but
/// not expanded *through*; `center` always expands.
pub(super) fn neighborhood_nodes(
    f: &Function,
    center: NodeId,
    depth: usize,
    hub_cap: usize,
    max_nodes: usize,
    consumers: &FxHashMap<NodeId, Vec<NodeId>>,
) -> FxHashSet<NodeId> {
    let mut seen = FxHashSet::default();
    seen.insert(center);
    let mut queue = VecDeque::from([(center, 0usize)]);
    'bfs: while let Some((node, dist)) = queue.pop_front() {
        if dist >= depth {
            continue;
        }
        let prod = producers(f, node);
        let cons = consumers.get(&node).cloned().unwrap_or_default();
        if node != center && prod.len() + cons.len() > hub_cap {
            continue; // hub: include, don't expand through
        }
        for nb in prod.into_iter().chain(cons) {
            if seen.len() >= max_nodes {
                break 'bfs; // budget spent; the nearest nodes are already in
            }
            if seen.insert(nb) {
                queue.push_back((nb, dist + 1));
            }
        }
    }
    seen
}

impl<R: MemReader> FunctionDotDumper<'_, R> {
    /// Standalone DOT for the depth-`depth` neighborhood around `center`, with
    /// `center` highlighted.
    ///
    /// # Errors
    /// Propagates a `pretty_label` IO error.
    pub fn neighborhood_dot(
        &self,
        center: NodeId,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
    ) -> io::Result<String> {
        let consumers = build_consumers(self.function);
        let set = neighborhood_nodes(self.function, center, depth, hub_cap, max_nodes, &consumers);
        let restricted = FunctionDotDumper {
            entry: self.entry,
            function: self.function,
            sleigh: self.sleigh,
            node_to_arg_indices: self.node_to_arg_indices.clone(),
            nodes: Some(set),
            center: Some(center),
        };
        ::dot::GraphDot::new(restricted, ::dot::DotStyle::dark())
            .as_dot()
            .map_err(|e| io::Error::other(e.to_string()))
    }
}
