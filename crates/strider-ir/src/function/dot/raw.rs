//! Raw, structure-faithful graph renderer for debugging.
//!
//! Unlike the pretty [`super::FunctionDotDumper`], this renders the graph
//! **exactly as stored**: one DOT node per reachable-from-entry [`NodeId`],
//! one edge per input edge, with no constant inlining, no synthetic virtual
//! nodes, no commutative
//! reordering, and no Sleigh register-name translation.  Each node also
//! shows the per-node side-table state ([`Function::stack_offset`],
//! the `value_vn` tag (via [`Function::get_vn_for_value`]),
//! [`Function::call_other_name`], a `Call`'s clobber-tag count, and
//! its argument index).  It is purely a debugging aid for inspecting the
//! real graph shape — pattern queries and the production pipeline use the
//! pretty renderer / the structured accessors instead.

use ::dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};
use rustc_hash::FxHashMap;

use crate::function::Function;
use crate::node::{NodeId, NodeKind};
use crate::{IRViewer, IRWalker};

/// Compact `Vn` rendering for raw labels: `{space-shortcut}{offset:#x}:{size}`
/// (e.g. `%0x38:8` for an 8-byte register, `#0x0:1` for a const), avoiding the
/// verbose derived `Vn { .. }` debug form.
fn fmt_vn(vn: &rsleigh::Vn) -> String {
    format!("{}{:#x}:{}", vn.addr_space.shortcut(), vn.addr_off, vn.size)
}

/// A raw 1:1 DOT dumper over a [`Function`] (see the module docs).
pub(super) struct RawFunctionDumper<'a> {
    function: &'a Function,
    /// Reverse of `Function::arg_index_to_values`: carrier node → arg indices.
    arg_index: FxHashMap<NodeId, Vec<u32>>,
}

impl<'a> RawFunctionDumper<'a> {
    /// Wraps `function` for raw rendering.
    pub(super) fn new(function: &'a Function) -> Self {
        Self {
            arg_index: super::build_arg_reverse_map(function),
            function,
        }
    }

    /// Builds the multi-line label for one node: id + kind + output kinds +
    /// every side-table entry recorded for it.
    fn node_label(&self, node: NodeId) -> String {
        let f = self.function;
        let kind = f.node_kind(node);
        // `InitialVar` carries an `all_vns` index; resolve it to the varnode
        // and render compactly (`#idx {space}{offset:#x}:{size}`) instead of
        // the verbose `Vn { .. }` debug.  Every other kind's debug form is
        // already terse.
        let kind_str = match kind {
            NodeKind::InitialVar(id) => match f.initial_vn_opt(*id) {
                Some(vn) => format!("InitialVar(#{} {})", id.index(), fmt_vn(&vn)),
                None => format!("InitialVar(#{} ?)", id.index()),
            },
            // Constants carry their value off-side in `const_interner`; show the
            // interner id compactly (`IntConst(#N)`) and the value below.
            NodeKind::IntConst(id) => format!("IntConst(#{})", id.as_u32()),
            other => format!("{other:?}"),
        };
        let mut s = format!("n{}  {kind_str}", node.as_u32());

        // Constants carry their value off-side in `const_interner`; show the
        // raw `ConstValue` debug (dangling ids labelled, not panicked).
        if let NodeKind::IntConst(id) = kind {
            let value = match f.const_interner.get(*id) {
                Some(cv) => format!("{cv:?}"),
                None => format!("<dangling const {id:?}>"),
            };
            s.push_str(&format!("\n= {value}"));
        }

        let outs: Vec<String> = f
            .node_outputs(node)
            .iter()
            .map(|o| format!("{:?}", f.value_kind(*o)))
            .collect();
        if !outs.is_empty() {
            s.push_str(&format!("\nout: {}", outs.join(", ")));
        }

        if let Some((_, off)) = f.stack_offset(node) {
            s.push_str(&format!("\nsp[{off}]"));
        }
        if let Some(vn) = f
            .node_outputs(node)
            .first()
            .copied()
            .and_then(|v| f.get_vn_for_value(v))
        {
            s.push_str(&format!("\ntag={}", fmt_vn(&vn)));
        }
        if let Some(name) = f.side_tables().call_other_name(node) {
            s.push_str(&format!("\nop={name}"));
        }
        if matches!(f.node_kind(node), NodeKind::Call) {
            // Call: show how many of its outputs carry a clobber varnode tag
            // (slots past Control/Memory).
            let tagged = f
                .node_outputs(node)
                .iter()
                .filter(|&&v| f.get_vn_for_value(v).is_some())
                .count();
            s.push_str(&format!("\nclobbers={tagged}"));
        }
        if let Some(indices) = self.arg_index.get(&node) {
            s.push_str(&format!("\narg{indices:?}"));
        }
        s
    }

    /// Raw render of the depth-`depth` neighborhood around `center` — the same
    /// BFS + `max_nodes` budget as the pretty neighborhood
    /// ([`super::FunctionDotDumper::neighborhood_dot`]), but structure-faithful:
    /// one `n<id>` box per node exactly as stored, edges as stored, no virtual
    /// nodes / const duplication / Sleigh names. The debug view for when the
    /// pretty output can't be trusted; `center` gets a bright border.
    fn neighborhood_dot(
        &self,
        center: NodeId,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
    ) -> anyhow::Result<String> {
        let consumers = super::neighborhood::build_consumers(self.function);
        let set = super::neighborhood::neighborhood_nodes(
            self.function,
            center,
            depth,
            hub_cap,
            max_nodes,
            &consumers,
        );
        let mut out = DotEmitter::new("G", &DotStyle::dark());
        for &node in &set {
            let dot_id = format!("n{}", node.as_u32());
            let extra: &[(&str, &str)] = if node == center {
                &[("color", "\"#ffcc00\""), ("penwidth", "2.5")]
            } else {
                &[]
            };
            out.node(&dot_id, &self.node_label(node), "box", extra);
        }
        for &node in &set {
            let dot_id = format!("n{}", node.as_u32());
            for (in_slot, value) in self.function.node_inputs(node).into_iter().enumerate() {
                let (producer, out_slot) = self.function.value_definition(value);
                if !set.contains(&producer) {
                    continue;
                }
                let from = format!("n{}", producer.as_u32());
                let label = format!("{out_slot}:{in_slot}");
                out.edge(&from, &dot_id, &[("label", &label)]);
            }
        }
        Ok(out.finish())
    }
}

impl GraphDotDumper for RawFunctionDumper<'_> {
    type Node = NodeId;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = NodeId> {
        // Nodes reachable from entry — the function's live graph.  Detached /
        // dedup-cache nodes are omitted so the view stays readable; because
        // the walk follows backward-data and forward-control edges, every
        // rendered node's input producers are also rendered (no dangling
        // edges).
        self.function.walk().collect::<Vec<_>>()
    }

    fn dump_as_dot(
        &self,
        node: NodeId,
        out: &mut DotEmitter,
        _state: &mut Self::State,
    ) -> anyhow::Result<()> {
        let dot_id = format!("n{}", node.as_u32());
        out.node(&dot_id, &self.node_label(node), "box", &[]);
        // One edge per input edge: producer-output-slot → consumer-input-slot.
        for (in_slot, value) in self.function.node_inputs(node).into_iter().enumerate() {
            let (producer, out_slot) = self.function.value_definition(value);
            let from = format!("n{}", producer.as_u32());
            // `edge` quotes+escapes the `label` value, so pass it bare.
            let label = format!("{out_slot}:{in_slot}");
            out.edge(&from, &dot_id, &[("label", &label)]);
        }
        Ok(())
    }
}

impl Function {
    /// Renders the graph **exactly as stored** to Graphviz DOT: every node
    /// reachable from entry, every input edge, side-tables shown inline, with
    /// none of the pretty renderer's cosmetic transforms (constant inlining,
    /// virtual nodes, commutative reordering).  A debugging aid for inspecting
    /// the real graph shape; see the `function::dot::raw` module.
    ///
    /// # Errors
    ///
    /// Propagates a DOT-emit error from the renderer.
    pub fn raw_dot(&self) -> crate::Result<String> {
        GraphDot::new(RawFunctionDumper::new(self), DotStyle::dark()).as_dot()
    }

    /// Like [`Self::raw_dot`] but wraps the DOT in a self-contained HTML page
    /// (embedded viz.js renderer — no external `dot` binary required).
    ///
    /// # Errors
    ///
    /// Propagates a DOT-emit error from the renderer.
    pub fn raw_html(&self) -> crate::Result<String> {
        GraphDot::new(RawFunctionDumper::new(self), DotStyle::dark()).as_html_from_dot()
    }

    /// Raw, structure-faithful render of the depth-`depth` neighborhood around
    /// `center` (see [`RawFunctionDumper::neighborhood_dot`]). The scale-safe
    /// debug counterpart to the pretty explorer view — no Sleigh needed.
    ///
    /// # Errors
    ///
    /// Propagates a DOT-emit error from the renderer.
    pub fn raw_neighborhood_dot(
        &self,
        center: NodeId,
        depth: usize,
        hub_cap: usize,
        max_nodes: usize,
    ) -> crate::Result<String> {
        RawFunctionDumper::new(self).neighborhood_dot(center, depth, hub_cap, max_nodes)
    }
}
