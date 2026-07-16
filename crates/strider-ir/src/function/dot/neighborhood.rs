//! Neighborhood selection for the interactive explorer.
//!
//! Picks the depth-N node set around a centre; the RENDER is the ordinary
//! pretty dumper restricted to that set (`FunctionDotDumper.nodes` /
//! `.center`), so styling, labels, edge roles, const-per-use boxes and the
//! `if.true` / `Post Call` virtuals are shared rather than reimplemented.
//!
//! A real node's DOT id is its IR `NodeId` (see
//! `FunctionDotDumperState::get_dot_id`), so the explorer navigates by id;
//! const (`c*`) and virtual (`v*`) boxes are not navigation targets, and
//! `FunctionDotDumperState::node_of_dot_id` resolves any of them back.

use std::collections::VecDeque;
use std::io;

use rsleigh::MemReader;
use rustc_hash::{FxHashMap, FxHashSet};

use super::FunctionDotDumper;
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
/// `center` always expands.
///
/// `max_nodes` bounds the total node count: because BFS visits in level order,
/// the budget keeps the *nearest* `max_nodes` nodes and stops. Depth alone
/// doesn't bound size — a densely-connected region blows up to hundreds of
/// nodes, which the browser's synchronous Graphviz layout can't render without
/// freezing — so the count cap is what actually keeps a neighborhood renderable.
/// Returns the set of nodes to draw.
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
            continue; // don't expand through a hub
        }
        for nb in prod.into_iter().chain(cons) {
            if seen.len() >= max_nodes {
                break 'bfs; // budget reached — keep the nearest max_nodes
            }
            if seen.insert(nb) {
                queue.push_back((nb, dist + 1));
            }
        }
    }
    seen
}

impl<R: MemReader> FunctionDotDumper<'_, R> {
    /// Renders the structural depth-`depth` neighborhood around `center` to a
    /// standalone DOT string, with `center` highlighted.  DOT node ids are IR
    /// node ids (see the module docs).
    ///
    /// # Errors
    /// Propagates a `pretty_label` IO error (e.g. a Sleigh register-name
    /// lookup failure).
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
