use rsleigh::MemReader;
use rustc_hash::FxHashMap;

use super::{
    GraphDotDumper, GraphDotDumperState, edge_style, node_fillcolor,
    node_shape,
};
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind};

/// Returns `true` when the node's single output has no uses.  In that case,
/// drawing the standalone node leaves an edgeless island beside the graph.
fn all_uses_go_through_inline(graph: &Graph, node: NodeId) -> bool {
    let outputs = graph.node_outputs(node);
    if outputs.len() != 1 {
        return false;
    }
    let out = outputs[0];
    graph.output_uses(out).count() == 0
}

impl<'a, R: MemReader> ::dot::GraphDotDumper for GraphDotDumper<'a, R> {
    type Node = crate::node::NodeId;
    type Error = std::io::Error;
    type State = GraphDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {
            visited_node_id: FxHashMap::default(),
            virtual_nodes: FxHashMap::default(),
            next_unique_id: 0,
        }
    }

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        // Walk from `entry`, then drop any node not in the active filter.
        // When no filter is set, every reachable node passes through.
        let walk: Vec<_> = self
            .function
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
        // Declare-this-node: skip-checks + node declaration.
        let Some(cur_id) = self.try_declare_node(node, out, state)? else {
            return Ok(());
        };
        let kind = *self.function.node_kind(node);

        // Emit per-output virtual nodes for If's true/false branches.
        if matches!(kind, NodeKind::If) {
            self.emit_if_branch_virtuals(node, &cur_id, out, state);
        }

        // Draw an edge from each input's producer (with any
        // virtual / inlined consumer-side helpers it needs).
        for (idx, parent_output) in self.function.node_inputs(node).into_iter().enumerate() {
            self.emit_input_edge(node, &cur_id, kind, idx, parent_output, out, state)?;
        }

        Ok(())
    }
}

impl<'a, R: MemReader> GraphDotDumper<'a, R> {
    /// First step of [`dump_as_dot`]: apply the skip-checks and emit
    /// the dot node declaration when the node passes.  Returns
    /// `Ok(Some(id))` (the dot id) when the node was declared,
    /// `Ok(None)` when it was filtered out or is a const (rendered
    /// inline beside its consumers) or an inlined-`InitialVar`.  An
    /// `Err` propagates a `pretty_label` IO failure to the caller
    /// (e.g. a Sleigh `vn_to_name` lookup that surfaces as
    /// `io::Error`).  Callers proceed to virtual-branch / edge-draw
    /// steps only on `Ok(Some(_))`.
    fn try_declare_node(
        &self,
        node: NodeId,
        out: &mut ::dot::DotEmitter,
        state: &mut GraphDotDumperState,
    ) -> std::io::Result<Option<String>> {
        // Defense in depth: even though `iter_nodes` filters, a caller
        // that drives `dump_as_dot` directly (e.g. some tests) might
        // still hand us an out-of-filter node.
        if !self.is_visible(node) {
            return Ok(None);
        }
        let kind = *self.function.node_kind(node);
        if kind.is_const() {
            return Ok(None);
        }
        // An `InitialVar` with no uses is rendered as floating edgeless,
        // so skip it.
        if matches!(kind, NodeKind::InitialVar(_))
            && all_uses_go_through_inline(self.function, node)
        {
            return Ok(None);
        }

        let cur_id = state.get_dot_id(self.function, node);

        // Build label: prepend "[arg N]" marker for FunctionArg carrier nodes.
        let base_label = self.pretty_label(node)?;
        let label = if let Some(indices) = self.node_to_arg_indices.get(&node) {
            let tag: String = indices
                .iter()
                .map(|i| format!("[arg {i}]"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{tag}\n{base_label}")
        } else {
            base_label
        };

        let fillcolor = node_fillcolor(&kind);

        // Double border for FunctionArg carrier nodes.
        let is_arg_node = self.node_to_arg_indices.contains_key(&node);
        let mut extra: Vec<(&str, &str)> = vec![("fillcolor", fillcolor)];
        if is_arg_node {
            extra.push(("peripheries", "2"));
        }

        out.node(&cur_id, &label, node_shape(&kind), &extra);
        Ok(Some(cur_id))
    }

    /// Emit "if.true" / "if.false" virtual trapezium nodes for the two
    /// control outputs of an If node so each branch is clearly
    /// labelled.  Reuses any virtual nodes a previously-rendered
    /// consumer already created eagerly.  Called by [`dump_as_dot`]
    /// after the If node itself is declared.
    fn emit_if_branch_virtuals(
        &self,
        node: NodeId,
        cur_id: &str,
        out: &mut ::dot::DotEmitter,
        state: &mut GraphDotDumperState,
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

    /// Get-or-create the per-If-branch "trapezium" virtual node keyed
    /// by `out_id` in `state.virtual_nodes`.  Either `emit_if_branch_virtuals`
    /// (driven by phase B when the If itself is rendered) or
    /// `emit_input_edge` (driven by phase C when a consumer is rendered
    /// before the producing If) can be the first to materialise the
    /// virtual; the get-or-create dance lets both paths share state.
    fn get_or_create_if_branch_virtual(
        state: &mut GraphDotDumperState,
        out_id: crate::node::NodeOutputId,
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

    /// Emit one edge from a single input's producer to `node`, plus
    /// any virtual / inlined producer-side helpers (inline `InitialVar`
    /// for SP slots, post-Call clobber virtuals, eager If branch
    /// virtuals when a consumer renders before its If producer) and
    /// inline const labels.  Called per input by [`dump_as_dot`].
    #[allow(clippy::too_many_arguments)]
    fn emit_input_edge(
        &self,
        node: NodeId,
        cur_id: &str,
        kind: NodeKind,
        idx: usize,
        parent_output: crate::node::NodeOutputId,
        out: &mut ::dot::DotEmitter,
        state: &mut GraphDotDumperState,
    ) -> core::result::Result<(), std::io::Error> {
        // Suppress the addr-input edge (slot 1) for Store and Load nodes whose
        // stack_offset side-table entry is populated.  The node label already
        // shows the offset (e.g. "[sp+0x10]"), so the edge to the SP-arithmetic
        // chain is visual noise.  The IR edge is untouched — rendering only.
        if idx == 1
            && matches!(kind, NodeKind::Store(_) | NodeKind::Load(_))
            && self.function.stack_offset(node).is_some()
        {
            return Ok(());
        }

        let parent_id = self.function.get_node_from_output(parent_output);
        // Skip edges whose producer was filtered out by the active
        // node filter.  Constants are always re-emitted alongside
        // their consumers (the `is_const` branch below), so they
        // bypass the filter check — the filter is for "real" graph
        // nodes, not inlined per-consumer constants.
        if !self.function.node_kind(parent_id).is_const() && !self.is_visible(parent_id) {
            return Ok(());
        }
        let parent_kind = *self.function.node_kind(parent_id);

        let inline_initial_var = false;

        // If the producing output has a virtual node, connect from
        // it.  For clobbered Call outputs (index >= 2), create the
        // virtual node on the fly the first time a consumer is
        // encountered.
        let parent_dot_id = if inline_initial_var {
            let v = state.alloc_virtual_id();
            self.emit_initial_var_node(parent_id, &v, out);
            v
        } else {
            let maybe_virt = state.virtual_nodes.get(&parent_output).cloned();
            if let Some(virt_id) = maybe_virt {
                virt_id
            } else if parent_kind == NodeKind::Call {
                let (_, output_index) = self.function.output_definition(parent_output);
                if output_index >= 2 {
                    let name = self.call_clobbered_name(parent_output)?;
                    let label = format!("Post Call\n{name}");
                    let virt_id = state.alloc_virtual_id();
                    let call_dot_id = state.get_dot_id(self.function,parent_id);
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
                    state.get_dot_id(self.function,parent_id)
                }
            } else if *self.function.node_kind(parent_id) == NodeKind::If {
                // The If node may not have been rendered yet.
                // Create the virtual branch node eagerly so this
                // consumer's edge lands on "if.true"/"if.false"
                // rather than directly on the If diamond, which
                // would leave the virtual node dangling.
                let (_, output_index) = self.function.output_definition(parent_output);
                let blabel = if output_index == 0 {
                    "if.true"
                } else {
                    "if.false"
                };
                Self::get_or_create_if_branch_virtual(state, parent_output, blabel, out)
            } else {
                state.get_dot_id(self.function,parent_id)
            }
        };

        let (label, color) = edge_style(self, node, idx, parent_output);

        // Numbered Call arg labels: inputs[0..2] are ctrl/mem/target,
        // so arg N lives at inputs[3 + N].  CallOther has no target,
        // so args start at inputs[2].  CPoolRef / New inputs are all
        // "ref N".
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
            // Partitioned memory edge: label with the alias class so the
            // reader can tell Stack from Unknown chains at a glance.
            // Applies to MemProject outputs and all downstream consumers
            // (MemUnion inputs, Load/Store mem slots, etc.).
            // Quote the label: it contains a colon, which is reserved
            // in unquoted dot identifiers (would be parsed as a port
            // specifier).  Other bare labels in this code path (ctrl,
            // arg0, mem) are valid unquoted identifiers so they don't
            // need quoting.
            self.function
                .output_kind(parent_output)
                .memory_partition()
                .map(|class| format!("\"mem:{}\"", class.as_str()))
        };
        let label_str: &str = owned_label.as_deref().unwrap_or(label);

        let mut extra: Vec<(&str, &str)> = vec![("color", color)];
        if !label_str.is_empty() {
            extra.push(("label", label_str));
            extra.push(("fontcolor", "\"#cccccc\""));
            extra.push(("fontsize", "9"));
        }

        out.edge(&parent_dot_id, cur_id, &extra);

        if self.function.node_kind(parent_id).is_const() {
            self.emit_const_node(parent_id, &parent_dot_id, out);
        }
        Ok(())
    }
}
