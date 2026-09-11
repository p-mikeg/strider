use rsleigh::MemReader;
use rustc_hash::FxHashMap;

use super::{FunctionDotDumper, FunctionDotDumperState, edge_style, node_fillcolor, node_shape};
use crate::IRViewer;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind};

/// Whether `node` has a single output with no uses.
fn all_uses_go_through_inline(graph: &Graph, node: NodeId) -> bool {
    let outputs = graph.node_outputs(node);
    if outputs.len() != 1 {
        return false;
    }
    let value = outputs[0];
    graph.value_uses(value).count() == 0
}

impl<'a, R: MemReader> ::dot::GraphDotDumper for FunctionDotDumper<'a, R> {
    type Node = crate::node::NodeId;
    type Error = std::io::Error;
    type State = FunctionDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {
            virtual_nodes: FxHashMap::default(),
            dot_to_node: FxHashMap::default(),
            next_unique_id: 0,
            center: self.center,
        }
    }

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        match &self.nodes {
            // NOT the walk filtered to the set: a neighbourhood is BFS'd from
            // its centre and need not be reachable from `entry`.
            Some(set) => {
                let mut v: Vec<NodeId> = set.iter().copied().collect();
                v.sort_unstable_by_key(|n| n.as_u32());
                v
            }
            None => crate::walk::walk_graph(self.function.graph(), self.entry).collect::<Vec<_>>(),
        }
    }

    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut ::dot::DotEmitter,
        state: &mut Self::State,
    ) -> core::result::Result<(), Self::Error> {
        let Some(cur_id) = self.try_declare_node(node, out, state)? else {
            return Ok(());
        };
        let kind = *self.function.node_kind(node);

        if matches!(kind, NodeKind::If) {
            self.emit_if_branch_virtuals(node, &cur_id, out, state);
        }

        for (idx, parent_value) in self.function.node_inputs(node).into_iter().enumerate() {
            self.emit_input_edge(node, &cur_id, kind, idx, parent_value, out, state)?;
        }

        Ok(())
    }
}

impl<'a, R: MemReader> FunctionDotDumper<'a, R> {
    /// `Ok(None)` when the node draws no shared box of its own (a per-use const,
    /// or a useless `InitialVar`).
    fn try_declare_node(
        &self,
        node: NodeId,
        out: &mut ::dot::DotEmitter,
        state: &mut FunctionDotDumperState,
    ) -> std::io::Result<Option<String>> {
        let kind = *self.function.node_kind(node);
        if state.renders_per_use(self.function.graph(), node) {
            return Ok(None);
        }
        if matches!(kind, NodeKind::InitialVar(_))
            && all_uses_go_through_inline(self.function.graph(), node)
        {
            return Ok(None);
        }

        let cur_id = state.get_dot_id(self.function.graph(), node);

        // Arg carrier nodes get an "[arg N]" prefix and a double border.
        let base_label = self.pretty_label(node)?;
        let label = if let Some(tags) = self.node_to_arg_indices.get(&node) {
            let tag: String = tags
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{tag}\n{base_label}")
        } else {
            base_label
        };

        let fillcolor = node_fillcolor(&kind);

        let is_arg_node = self.node_to_arg_indices.contains_key(&node);
        let mut extra: Vec<(&str, &str)> = vec![("fillcolor", fillcolor)];
        if is_arg_node {
            extra.push(("peripheries", "2"));
        }
        if self.center == Some(node) {
            extra.push(("color", "\"#ffcc00\""));
            extra.push(("penwidth", "2.5"));
        }

        out.node(&cur_id, &label, node_shape(&kind), &extra);
        Ok(Some(cur_id))
    }

    /// Reuses any virtual a previously-rendered consumer already created.
    fn emit_if_branch_virtuals(
        &self,
        node: NodeId,
        cur_id: &str,
        out: &mut ::dot::DotEmitter,
        state: &mut FunctionDotDumperState,
    ) {
        let outputs = self.function.node_outputs(node);
        let branch_labels = ["if.true", "if.false"];
        let edge_labels = ["true", "false"];
        for ((out_id, blabel), elabel) in outputs
            .iter()
            .copied()
            .zip(branch_labels.iter())
            .zip(edge_labels.iter())
        {
            let virt_id = Self::get_or_create_if_branch_virtual(state, out_id, blabel, out);
            out.edge(
                cur_id,
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

    /// The `if.true` / `if.false` virtual for `out_id`, created on first call.
    fn get_or_create_if_branch_virtual(
        state: &mut FunctionDotDumperState,
        out_id: crate::node::ValueId,
        blabel: &str,
        out: &mut ::dot::DotEmitter,
    ) -> String {
        match state.virtual_nodes.get(&out_id).cloned() {
            Some(existing) => existing,
            None => {
                let v = state.alloc_virtual_id();
                out.node(&v, blabel, "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
                state.virtual_nodes.insert(out_id, v.clone());
                v
            }
        }
    }

    /// One edge from an input's producer to `node`, materialising whatever
    /// producer-side helper it needs.
    #[allow(clippy::too_many_arguments)]
    fn emit_input_edge(
        &self,
        node: NodeId,
        cur_id: &str,
        kind: NodeKind,
        idx: usize,
        parent_value: crate::node::ValueId,
        out: &mut ::dot::DotEmitter,
        state: &mut FunctionDotDumperState,
    ) -> core::result::Result<(), std::io::Error> {
        let parent_id = self.function.producer(parent_value);
        // Restricted render: drop edges whose producer is out of view.
        if self
            .nodes
            .as_ref()
            .is_some_and(|set| !set.contains(&parent_id))
        {
            return Ok(());
        }
        let parent_kind = *self.function.node_kind(parent_id);

        // Connect from the producing output's virtual node if it has one;
        // clobbered Call outputs (index >= 2) get theirs made on first use.
        let parent_dot_id = {
            let maybe_virt = state.virtual_nodes.get(&parent_value).cloned();
            if let Some(virt_id) = maybe_virt {
                virt_id
            } else if parent_kind == NodeKind::Call {
                let (_, output_index) = self.function.value_definition(parent_value);
                if output_index >= 2 {
                    let name = self.call_clobbered_name(parent_value)?;
                    let label = format!("Post Call\n{name}");
                    let virt_id = state.alloc_virtual_id();
                    let call_dot_id = state.get_dot_id(self.function.graph(), parent_id);
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
                    state.virtual_nodes.insert(parent_value, virt_id.clone());
                    virt_id
                } else {
                    state.get_dot_id(self.function.graph(), parent_id)
                }
            } else if *self.function.node_kind(parent_id) == NodeKind::If {
                // The If may not have rendered yet, so make the branch virtual
                // eagerly rather than landing this edge on the If diamond.
                let (_, output_index) = self.function.value_definition(parent_value);
                let blabel = if output_index == 0 {
                    "if.true"
                } else {
                    "if.false"
                };
                Self::get_or_create_if_branch_virtual(state, parent_value, blabel, out)
            } else {
                state.get_dot_id(self.function.graph(), parent_id)
            }
        };

        let (label, color) = edge_style(self, node, idx, parent_value);

        // Call inputs are [ctrl, mem, target, sp, args...], so arg N is at
        // 4 + N.  CallOther has no target slot, so its args start at 2.
        let owned_label: Option<String> = if matches!(kind, NodeKind::Call) && idx == 3 {
            Some("sp".to_owned())
        } else if matches!(kind, NodeKind::Call) && idx >= 4 {
            Some(format!("arg{}", idx - 4))
        } else if matches!(kind, NodeKind::CallOther { .. }) && idx >= 2 {
            Some(format!("arg{}", idx - 2))
        } else if matches!(kind, NodeKind::CPoolRef | NodeKind::New) {
            Some(format!("ref{idx}"))
        } else if matches!(kind, NodeKind::Return) && idx >= 2 {
            // Slots 2.. are the convention's return registers in ABI order.
            self.return_ret_name(idx)?
        } else {
            pred_index(kind, idx).map(|pred| format!("pred{pred}"))
        };
        let label_str: &str = owned_label.as_deref().unwrap_or(label);

        let mut extra: Vec<(&str, &str)> = vec![("color", color)];
        if !label_str.is_empty() {
            extra.push(("label", label_str));
            extra.push(("fontcolor", "\"#cccccc\""));
            extra.push(("fontsize", "9"));
        }

        out.edge(&parent_dot_id, cur_id, &extra);

        if state.renders_per_use(self.function.graph(), parent_id) {
            self.emit_const_node(parent_id, &parent_dot_id, out);
        }
        Ok(())
    }
}

/// Predecessor index for an input that pairs across Region / Phi / MemPhi at a
/// common join.  `None` for any other input.
fn pred_index(kind: NodeKind, idx: usize) -> Option<usize> {
    match kind {
        NodeKind::Region => Some(idx),
        NodeKind::Phi | NodeKind::MemPhi if idx >= 1 => Some(idx - 1),
        _ => None,
    }
}
