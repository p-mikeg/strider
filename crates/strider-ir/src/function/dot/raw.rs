//! Raw, structure-faithful graph renderer for debugging.
//!
//! Unlike the pretty [`super::FunctionDotDumper`], this renders the graph
//! **exactly as stored**: one DOT node per reachable-from-entry [`NodeId`],
//! one edge per input edge, with no constant inlining, no synthetic virtual
//! nodes, no commutative
//! reordering, and no Sleigh register-name translation.  Each node also
//! shows the per-node side-table state ([`Function::stack_offset`],
//! the `value_vn` tag (via [`Function::get_vn_for_value`]), [`Function::asm_fingerprint`],
//! [`Function::call_other_name`], [`Function::call_cc`], and
//! its argument index).  It is purely a debugging aid for inspecting the
//! real graph shape — pattern queries and the production pipeline use the
//! pretty renderer / the structured accessors instead.

use ::dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};
use rustc_hash::FxHashMap;

use crate::{
    IRViewer, IRWalker,
    function::Function,
    node::{NodeId, NodeKind},
};

/// Compact `Vn` rendering for raw labels: `{space-shortcut}{offset:#x}:{size}`
/// (e.g. `%0x38:8` for an 8-byte register, `#0x0:1` for a const), avoiding the
/// verbose derived `Vn { .. }` debug form.
fn fmt_vn(vn: &rsleigh::Vn) -> String {
    format!("{}{:#x}:{}", vn.addr_space.shortcut(), vn.addr_off, vn.size)
}

/// A raw 1:1 DOT dumper over a [`Function`] (see the module docs).
pub struct RawFunctionDumper<'a> {
    function: &'a Function,
    /// Reverse of `Function::arg_index_to_values`: carrier node → arg indices.
    arg_index: FxHashMap<NodeId, Vec<u32>>,
}

impl<'a> RawFunctionDumper<'a> {
    /// Wraps `function` for raw rendering.
    pub fn new(function: &'a Function) -> Self {
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
            let value = match f.const_value_opt(*id) {
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
        let fp = f.asm_fingerprint(node);
        if !fp.is_empty() {
            let addrs: Vec<String> = fp.iter().map(|a| format!("{a:#x}")).collect();
            s.push_str(&format!("\nfp=[{}]", addrs.join(",")));
        }
        if let Some(name) = f.call_other_name(node) {
            s.push_str(&format!("\nop={name}"));
        }
        if f.call_cc(node).is_some() {
            // Override Call: show how many of its outputs carry a clobber
            // varnode tag (slots past Control/Memory).
            let tagged = f
                .node_outputs(node)
                .iter()
                .filter(|&&v| f.get_vn_for_value(v).is_some())
                .count();
            s.push_str(&format!("\ncall_cc(clobbers={tagged})"));
        }
        if let Some(indices) = self.arg_index.get(&node) {
            s.push_str(&format!("\narg{indices:?}"));
        }
        s
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
            // Integer-only label (`out_slot:in_slot`) — safe for `edge`'s
            // unescaped attribute channel.
            let label = format!("\"{out_slot}:{in_slot}\"");
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
}
