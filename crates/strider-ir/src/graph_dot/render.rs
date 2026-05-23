use rsleigh::MemReader;
use std::collections::HashMap;

use super::{GraphDotDumper, GraphDotDumperState, edge_style, node_fillcolor, node_shape};
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind};

/// Returns `true` when every use of `node`'s single output is the SP input of
/// a `StackStore`/`StackStorePhi`, which the renderer inlines as a per-consumer
/// virtual node.  Also `true` when the output has no uses at all — either
/// way, drawing the standalone node leaves an edgeless island beside the
/// graph.
fn all_uses_go_through_inline(graph: &Graph, node: NodeId) -> bool {
    let outputs = graph.node_outputs(node);
    if outputs.len() != 1 {
        return false;
    }
    let out = outputs[0];
    graph.output_uses(out).all(|(consumer, idx)| {
        idx == 1
            && matches!(
                graph.node_kind(consumer),
                NodeKind::StackStore { .. } | NodeKind::StackStorePhi { .. }
            )
    })
}

impl<'a, R: MemReader> ::dot::GraphDotDumper for GraphDotDumper<'a, R> {
    type Node = crate::node::NodeId;
    type Error = std::io::Error;
    type State = GraphDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {
            visited_node_id: HashMap::new(),
            virtual_nodes: HashMap::new(),
            next_unique_id: 0,
        }
    }

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        // Walk from `entry`, then drop any node not in the active filter.
        // When no filter is set, every reachable node passes through.
        let walk: Vec<_> = self
            .graph
            .walk_from(self.entry)
            .filter(|n| self.is_visible(*n))
            .collect();
        walk
    }

    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut ::dot::DotEmitter,
        state: &mut Self::State,
    ) -> core::result::Result<(), Self::Error> {
        // Defense in depth: even though `iter_nodes` filters, a caller
        // that drives `dump_as_dot` directly (e.g. some tests) might
        // still hand us an out-of-filter node.
        if !self.is_visible(node) {
            return Ok(());
        }
        if self.graph.node_kind(node).is_const() {
            return Ok(());
        }

        let kind = self.graph.node_kind(node);

        // An `InitialVar` whose sole consumer is a `StackStore`/`StackStorePhi`
        // SP input is rendered inline as a virtual copy beside each consumer
        // (see `inline_initial_var` below).  Emitting the real node in that
        // case leaves it floating edgeless, so skip it.
        if matches!(kind, NodeKind::InitialVar(_))
            && all_uses_go_through_inline(self.graph, node)
        {
            return Ok(());
        }

        let cur_id = state.get_dot_id(self.graph, node);
        let shape = node_shape(kind);
        let fc = node_fillcolor(kind);

        out.node(
            &cur_id,
            &self.pretty_label(node)?,
            shape,
            &[("fillcolor", fc)],
        );

        // ── Virtual nodes for structured outputs ──────────────────────────────

        // For the two control outputs of an If node, emit "if.true" and
        // "if.false" virtual nodes so each branch is clearly labelled.
        if matches!(kind, NodeKind::If) {
            let outputs = self.graph.node_outputs(node);
            let branch_labels = ["if.true", "if.false"];
            let edge_labels = ["true", "false"];
            for ((out_id, blabel), elabel) in outputs
                .iter()
                .copied()
                .zip(branch_labels.iter())
                .zip(edge_labels.iter())
            {
                // A consumer rendered before this If may have already created
                // the virtual node eagerly.  Reuse it to avoid a duplicate
                // declaration; only emit `node` when creating for the first time.
                let virt_id = match state.virtual_nodes.get(&out_id).cloned() {
                    Some(existing) => existing,
                    None => {
                        let v = state.alloc_virtual_id();
                        out.node(&v, blabel, "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
                        state.virtual_nodes.insert(out_id, v.clone());
                        v
                    }
                };
                out.edge(
                    &cur_id,
                    &virt_id,
                    &[
                        ("color", "\"#00cccc\""),
                        ("label", elabel),
                        ("fontcolor", "\"#cccccc\""),
                        ("fontsize", "9"),
                    ],
                );
            }
        }

        // ── Draw edges from this node's inputs to this node ───────────────────

        for (idx, parent_output) in self.graph.node_inputs(node).into_iter().enumerate() {
            let parent_id = self.graph.get_node_from_output(parent_output);
            // Skip edges whose producer was filtered out by the active
            // node filter.  Constants are always re-emitted alongside
            // their consumers (the `is_const` branch below), so they
            // bypass the filter check — the filter is for "real" graph
            // nodes, not inlined per-consumer constants.
            if !self.graph.node_kind(parent_id).is_const() && !self.is_visible(parent_id) {
                continue;
            }
            let parent_kind = *self.graph.node_kind(parent_id);

            // Inline the SP `InitialVar` into each StackStore/StackStorePhi
            // consumer: otherwise every stack store edges back to a single
            // shared node, which turns the graph into a visual hub.
            let inline_initial_var = matches!(parent_kind, NodeKind::InitialVar(_))
                && matches!(
                    kind,
                    NodeKind::StackStore { .. } | NodeKind::StackStorePhi { .. }
                )
                && idx == 1;

            // If the producing output has a virtual node, connect from it.
            // For clobbered Call outputs (index >= 2), create the virtual node
            // on the fly the first time a consumer is encountered.
            let parent_dot_id = if inline_initial_var {
                let v = state.alloc_virtual_id();
                self.emit_initial_var_node(parent_id, &v, out);
                v
            } else {
                let maybe_virt = state.virtual_nodes.get(&parent_output).cloned();
                if let Some(virt_id) = maybe_virt {
                    virt_id
                } else if parent_kind == NodeKind::Call {
                    let (_, output_index) = self.graph.output_definition(parent_output);
                    if output_index >= 2 {
                        let name = self.call_clobbered_name(parent_output)?;
                        let label = format!("Post Call\n{name}");
                        let virt_id = state.alloc_virtual_id();
                        let call_dot_id = state.get_dot_id(self.graph, parent_id);
                        out.node(
                            &virt_id,
                            &label,
                            "box",
                            &[("fillcolor", "\"#28102a\""), ("style", "\"filled,dashed\"")],
                        );
                        out.edge(
                            &call_dot_id,
                            &virt_id,
                            &[("color", "\"#888888\""), ("style", "dashed")],
                        );
                        state.virtual_nodes.insert(parent_output, virt_id.clone());
                        virt_id
                    } else {
                        state.get_dot_id(self.graph, parent_id)
                    }
                } else if *self.graph.node_kind(parent_id) == NodeKind::If {
                    // The If node may not have been rendered yet.  Create the
                    // virtual branch node eagerly so this consumer's edge lands
                    // on "if.true"/"if.false" rather than directly on the If
                    // diamond, which would leave the virtual node dangling.
                    let (_, output_index) = self.graph.output_definition(parent_output);
                    let blabel = if output_index == 0 {
                        "if.true"
                    } else {
                        "if.false"
                    };
                    match state.virtual_nodes.get(&parent_output).cloned() {
                        Some(existing) => existing,
                        None => {
                            let v = state.alloc_virtual_id();
                            out.node(&v, blabel, "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
                            state.virtual_nodes.insert(parent_output, v.clone());
                            v
                        }
                    }
                } else {
                    state.get_dot_id(self.graph, parent_id)
                }
            };

            let (label, color) = edge_style(self, node, idx, parent_output);

            // Numbered Call arg labels: inputs[0..2] are ctrl/mem/target,
            // so arg N lives at inputs[3 + N].  CallOther has no target, so
            // args start at inputs[2].  CPoolRef / New inputs are all "ref N".
            let owned_label: Option<String> = if matches!(kind, NodeKind::Call) && idx >= 3 {
                Some(format!("arg{}", idx - 3))
            } else if matches!(kind, NodeKind::CallOther { .. }) && idx >= 2 {
                Some(format!("arg{}", idx - 2))
            } else if matches!(kind, NodeKind::CPoolRef | NodeKind::New) {
                Some(format!("ref{idx}"))
            } else if matches!(kind, NodeKind::Return) && idx >= 2 {
                // Return ret-val input slots (2..) carry the calling
                // convention's return registers in ABI order.  Label with
                // the vn name if we know it; fall back to the signature's
                // generic "ret" label otherwise.
                self.return_ret_name(idx)?
            } else {
                None
            };
            let label_str: &str = owned_label.as_deref().unwrap_or(label);

            let mut extra: Vec<(&str, &str)> = vec![("color", color)];
            if !label_str.is_empty() {
                extra.push(("label", label_str));
                extra.push(("fontcolor", "\"#cccccc\""));
                extra.push(("fontsize", "9"));
            }

            out.edge(&parent_dot_id, &cur_id, &extra);

            if self.graph.node_kind(parent_id).is_const() {
                self.emit_const_node(parent_id, &parent_dot_id, out);
            }
        }

        Ok(())
    }
}
