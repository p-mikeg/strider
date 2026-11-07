use std::collections::HashSet;

use rustc_hash::FxHashMap;

use super::FunctionDotDumper;
use crate::function::Function;
use crate::node::NodeId;
use crate::{IRViewer, IRWalker};

/// Consumers per producer among the nodes `f.walk()` reaches, in walk order;
/// `value_uses` would also yield dead consumers on an uncompacted graph.
fn build_consumers(f: &Function) -> FxHashMap<NodeId, Vec<NodeId>> {
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

/// BFS around `center` over input and output edges, capped at `max_nodes`. A
/// hub (consumer fan-out, plus producers when `count_producers`, over `hub_cap`)
/// is included but its consumers are not followed, except at `center`.
pub(super) fn neighborhood_nodes(
    f: &Function,
    center: NodeId,
    depth: usize,
    hub_cap: usize,
    max_nodes: usize,
    count_producers: bool,
) -> HashSet<NodeId> {
    let consumers = build_consumers(f);
    ::dot::nearest(center, depth, max_nodes, |node| {
        let mut nbs = producers(f, node);
        let cons = consumers.get(&node).map_or(&[][..], Vec::as_slice);
        let degree = cons.len() + if count_producers { nbs.len() } else { 0 };
        if node == center || degree <= hub_cap {
            nbs.extend_from_slice(cons);
        }
        nbs
    })
}

impl FunctionDotDumper<'_> {
    /// Standalone DOT for the nodes within `depth` edges of `center`, `center`
    /// highlighted.
    ///
    /// # Errors
    /// Propagates a DOT-emit error from the renderer.
    pub fn neighborhood_dot(
        self,
        center: NodeId,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
        count_producers: bool,
    ) -> crate::Result<String> {
        let set = neighborhood_nodes(
            self.function,
            center,
            depth,
            hub_cap,
            max_nodes,
            count_producers,
        );
        let restricted = FunctionDotDumper {
            view: Some(::dot::View::new(set, center)),
            ..self
        };
        ::dot::GraphDot::new(restricted, ::dot::DotStyle::dark()).as_dot()
    }
}
