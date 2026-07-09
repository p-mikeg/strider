//! Neighborhood renderer for the interactive explorer.
//!
//! Renders the depth-N neighborhood around a node as DOT, reusing the pretty
//! dumper's styling / labels / edge roles ([`super::node_shape`],
//! [`super::node_fillcolor`], [`super::edge_style`],
//! [`FunctionDotDumper::pretty_label`]) *and* its `if.true` / `if.false` and
//! `Post Call` virtual nodes — but keeps every **real** node's DOT id equal to
//! its IR `NodeId` (constants included; the pretty dumper inlines those and
//! renumbers). Virtual nodes get `v_*` ids. That way real nodes map 1:1 to the
//! IR, so the explorer can highlight pattern-match roots and navigate by node
//! id, while virtual `v_*` nodes are simply not navigation targets.

use std::collections::VecDeque;
use std::io;

use rsleigh::MemReader;
use rustc_hash::{FxHashMap, FxHashSet};

use super::{FunctionDotDumper, edge_style, node_fillcolor, node_shape};
use crate::function::Function;
use crate::node::{NodeId, NodeKind};
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
        max_nodes: usize,
    ) -> io::Result<String> {
        let consumers = build_consumers(self.function);
        let set = neighborhood_nodes(self.function, center, depth, hub_cap, max_nodes, &consumers);

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
        // One edge per IR input edge whose producer is in the set, colored and
        // labeled by the consumer's input-slot role. Two producer kinds route
        // through virtual nodes, mirroring the pretty dumper: an `If`'s control
        // outputs go via an `if.true` / `if.false` trapezium, and a `Call`'s
        // clobbered-register outputs (slot ≥ 2) via a dashed `Post Call\n<reg>`
        // box. Virtual ids are `v_<valueid>` so they never collide with the
        // integer IR-node ids (which is what the explorer navigates by).
        let mut virt: FxHashMap<crate::node::ValueId, String> = FxHashMap::default();
        for &node in &set {
            let id = node.as_u32().to_string();
            for (idx, value) in self.function.node_inputs(node).into_iter().enumerate() {
                let (producer, out_slot) = self.function.value_definition(value);
                if !set.contains(&producer) {
                    continue;
                }
                let src_id = match self.function.node_kind(producer) {
                    NodeKind::If => virt
                        .entry(value)
                        .or_insert_with(|| {
                            let vid = format!("v_{}", value.as_u32());
                            let label = if out_slot == 0 { "if.true" } else { "if.false" };
                            out.node(&vid, label, "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
                            out.edge(
                                &producer.as_u32().to_string(),
                                &vid,
                                &[("color", "\"#888888\"")],
                            );
                            vid
                        })
                        .clone(),
                    NodeKind::Call if out_slot >= 2 => match virt.get(&value) {
                        Some(v) => v.clone(),
                        None => {
                            let vid = format!("v_{}", value.as_u32());
                            let name = self.call_clobbered_name(value)?;
                            out.node(
                                &vid,
                                &format!("Post Call\n{name}"),
                                "box",
                                &[("fillcolor", "\"#28102a\""), ("style", "\"filled,dashed\"")],
                            );
                            out.edge(
                                &producer.as_u32().to_string(),
                                &vid,
                                &[("color", "\"#888888\""), ("style", "dashed")],
                            );
                            virt.insert(value, vid.clone());
                            vid
                        }
                    },
                    _ => producer.as_u32().to_string(),
                };
                let (slot, color) = edge_style(self, node, idx, value);
                let mut attrs: Vec<(&str, &str)> = vec![("color", color)];
                if !slot.is_empty() {
                    attrs.push(("label", slot));
                    attrs.push(("fontcolor", color));
                    attrs.push(("fontsize", "9"));
                }
                out.edge(&src_id, &id, &attrs);
            }
        }
        Ok(out.finish())
    }
}
