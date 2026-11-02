use ::dot::GraphDotDumper as _;

use super::{FunctionDotDumper, FunctionDotDumperState, edge_style, node_style};
use crate::IRViewer;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind};

/// Whether `node` has a single output with no uses.
fn has_one_unused_output(graph: &Graph, node: NodeId) -> bool {
    let outputs = graph.node_outputs(node);
    if outputs.len() != 1 {
        return false;
    }
    graph.value_uses(outputs[0]).next().is_none()
}

impl ::dot::GraphDotDumper for FunctionDotDumper<'_> {
    type Node = crate::node::NodeId;
    type Error = std::io::Error;
    type State = FunctionDotDumperState;

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        crate::walk::walk_graph(self.function.graph(), self.function.entry())
    }

    fn view(&self) -> Option<&::dot::View<NodeId>> {
        self.view.as_ref()
    }

    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut ::dot::DotEmitter,
        state: &mut Self::State,
    ) -> core::result::Result<(), Self::Error> {
        let Some(cur_id) = self.try_declare_node(node, out, state) else {
            return Ok(());
        };
        let kind = *self.function.node_kind(node);

        if matches!(kind, NodeKind::If) {
            self.emit_if_branch_virtuals(node, &cur_id, out, state);
        }

        for (idx, parent_value) in self.function.node_inputs(node).into_iter().enumerate() {
            self.emit_input_edge(node, &cur_id, kind, idx, parent_value, out, state);
        }

        Ok(())
    }
}

impl FunctionDotDumper<'_> {
    /// `Ok(None)` when the node draws no shared box of its own (a per-use const,
    /// or a useless `InitialVar`).
    fn try_declare_node(
        &self,
        node: NodeId,
        out: &mut ::dot::DotEmitter,
        state: &mut FunctionDotDumperState,
    ) -> Option<String> {
        let kind = *self.function.node_kind(node);
        if self.renders_per_use(node) {
            return None;
        }
        if matches!(kind, NodeKind::InitialVar(_))
            && has_one_unused_output(self.function.graph(), node)
        {
            return None;
        }

        let cur_id = self.dot_id(state, node);

        // Arg carrier nodes get an "[arg N]" prefix and a double border.
        let base_label = self.pretty_label(node);
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

        let (shape, fillcolor) = node_style(&kind);

        let is_arg_node = self.node_to_arg_indices.contains_key(&node);
        let mut extra: Vec<(&str, &str)> = vec![("fillcolor", fillcolor)];
        if is_arg_node {
            extra.push(("peripheries", "2"));
        }
        extra.extend_from_slice(self.center_attrs(node));

        out.node(&cur_id, &label, shape, &extra);
        Some(cur_id)
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
                    ("color", "#00cccc"),
                    ("label", elabel),
                    ("fontcolor", "#cccccc"),
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
                out.node(&v, blabel, "trapezium", &[("fillcolor", "#3a2a10")]);
                state.virtual_nodes.insert(out_id, v.clone());
                v
            }
        }
    }

    /// One edge from an input's producer to `node`, materialising whatever
    /// producer-side helper it needs.
    // The arguments are one render cursor; a struct for them would only be
    // filled and read at this one call site.
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
    ) {
        let parent_id = self.function.producer(parent_value);
        if !self.in_view(parent_id) {
            return;
        }
        let parent_kind = *self.function.node_kind(parent_id);

        // Connect from the producing output's virtual node if it has one;
        // clobbered Call outputs (index >= 2) get theirs made on first use.
        let parent_dot_id = {
            let maybe_virt = state.virtual_nodes.get(&parent_value).cloned();
            if let Some(virt_id) = maybe_virt {
                virt_id
            } else if matches!(parent_kind, NodeKind::Call { .. }) {
                let (_, output_index) = self.function.value_definition(parent_value);
                if output_index >= 2 {
                    let name = self.call_clobbered_name(parent_value);
                    let label = format!("Post Call\n{name}");
                    let virt_id = state.alloc_virtual_id();
                    let call_dot_id = self.dot_id(state, parent_id);
                    out.node(
                        &virt_id,
                        &label,
                        "box",
                        &[("fillcolor", "#28102a"), ("style", "filled,dashed")],
                    );
                    out.edge(
                        &call_dot_id,
                        &virt_id,
                        &[("color", "#888888"), ("style", "dashed")],
                    );
                    state.virtual_nodes.insert(parent_value, virt_id.clone());
                    virt_id
                } else {
                    self.dot_id(state, parent_id)
                }
            } else if parent_kind == NodeKind::If {
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
                self.dot_id(state, parent_id)
            }
        };

        let (label, tail_idx, color) = edge_style(self, node, idx);

        // Every repeating input is `<slot name><ordinal>`, the ordinal from
        // the signature's own head length. Two kinds read better otherwise:
        // a `Return`'s tail slots are the convention's return registers in ABI
        // order, and a control predecessor is named by its ordinal among the
        // `Region`'s arms, which is what pairs it with a `Phi` input.
        let owned_label: Option<String> = match tail_idx {
            Some(_) if matches!(kind, NodeKind::Return) => self.return_ret_name(idx),
            Some(n) if matches!(kind, NodeKind::Region | NodeKind::Phi | NodeKind::MemPhi) => {
                Some(format!("pred{n}"))
            }
            Some(n) => Some(format!("{label}{n}")),
            None => None,
        };
        let label_str: &str = owned_label.as_deref().unwrap_or(label);

        let mut extra: Vec<(&str, &str)> = vec![("color", color)];
        if !label_str.is_empty() {
            extra.push(("label", label_str));
            extra.push(("fontcolor", "#cccccc"));
            extra.push(("fontsize", "9"));
        }

        out.edge(&parent_dot_id, cur_id, &extra);

        if self.renders_per_use(parent_id) {
            self.emit_const_node(parent_id, &parent_dot_id, out);
        }
    }
}
