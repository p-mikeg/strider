//! Raw graph renderer for debugging: the graph **exactly as stored**, one DOT
//! node per reachable-from-entry [`NodeId`], one edge per input edge, and
//! side-table state inline.

use ::dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};
use dashu_int::UBig;
use rustc_hash::FxHashMap;

use crate::function::Function;
use crate::node::{NodeId, NodeKind};
use crate::{IRViewer, IRWalker};

/// The raw label's spelling of an interned constant: the decimal value at any
/// width, never a storage form. Rendered graphs are compared across builds, so
/// this text is an artifact.
pub(super) fn const_label(value: &UBig) -> String {
    format!("{value}")
}

/// `{space}{offset:#x}:{size}`, the space as the text format spells it, e.g.
/// `%0x38:8`.
fn fmt_vn(vn: &rsleigh::Vn) -> String {
    format!(
        "{}{:#x}:{}",
        crate::function::space_str(vn.addr_space),
        vn.addr_off,
        vn.size
    )
}

pub(super) struct RawFunctionDumper<'a> {
    function: &'a Function,
    /// Carrier node -> its arg tags, joined.
    arg_labels: FxHashMap<NodeId, String>,
    /// A neighbourhood render; `None` dumps the whole walk.
    view: Option<::dot::View<NodeId>>,
}

impl<'a> RawFunctionDumper<'a> {
    pub(super) fn new(function: &'a Function) -> Self {
        Self {
            arg_labels: super::arg_labels(function),
            function,
            view: None,
        }
    }

    /// id + kind + output kinds + every side-table entry recorded for the node.
    fn node_label(&self, node: NodeId) -> String {
        let f = self.function;
        let kind = f.node_kind(node);
        // `InitialVar` carries an index; resolve it to the varnode.
        let kind_str = match kind {
            NodeKind::InitialVar(id) => match f.initial_vn_opt(*id) {
                Some(vn) => format!("InitialVar(#{} {})", id.as_u32(), fmt_vn(&vn)),
                None => format!("InitialVar(#{} ?)", id.as_u32()),
            },
            // The value itself goes on the next line.
            NodeKind::IntConst(id) => format!("IntConst(#{})", id.as_u32()),
            NodeKind::Switch(_) => "Switch".to_string(),
            NodeKind::Call { .. } => "Call".to_string(),
            NodeKind::Entry
            | NodeKind::InitialMemory
            | NodeKind::Region
            | NodeKind::MemPhi
            | NodeKind::Phi
            | NodeKind::If
            | NodeKind::Return
            | NodeKind::IndirectBranch
            | NodeKind::Unreachable
            | NodeKind::Load(_)
            | NodeKind::Store(_)
            | NodeKind::IntUnaryOp(_)
            | NodeKind::IntBinaryOp(_)
            | NodeKind::IntCmpOp(_)
            | NodeKind::Truncate
            | NodeKind::Popcount
            | NodeKind::Lzcount
            | NodeKind::Extend(_)
            | NodeKind::FloatConst(_)
            | NodeKind::FloatBinaryOp(_)
            | NodeKind::FloatUnaryOp(_)
            | NodeKind::FloatCmpOp(_)
            | NodeKind::IntToFloat
            | NodeKind::FloatToInt
            | NodeKind::FloatToFloat
            | NodeKind::IntBitsToFloat
            | NodeKind::FloatBitsToInt
            | NodeKind::CallOther { .. }
            | NodeKind::CPoolRef
            | NodeKind::New => format!("{kind:?}"),
        };
        let mut s = format!("n{}  {kind_str}", node.as_u32());

        if let NodeKind::IntConst(id) = kind {
            let value = match f.side_tables().const_interner.get(*id) {
                Some(cv) => const_label(cv),
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
        if let Some(name) = f.call_other_name(node) {
            s.push_str(&format!("\nop={name}"));
        }
        if matches!(kind, NodeKind::Call { .. }) {
            // How many outputs past Control/Memory carry a clobber tag.
            let tagged = f
                .node_outputs(node)
                .iter()
                .filter(|&&v| f.get_vn_for_value(v).is_some())
                .count();
            s.push_str(&format!("\nclobbers={tagged}"));
        }
        if let Some(tags) = self.arg_labels.get(&node) {
            s.push_str(&format!("\n{tags}"));
        }
        s
    }
}

impl GraphDotDumper for RawFunctionDumper<'_> {
    type Node = NodeId;
    type State = ();

    /// The walk follows backward-data and forward-control edges, so every
    /// rendered node's input producers are rendered too: no dangling edges.
    fn iter_nodes(&self) -> impl IntoIterator<Item = NodeId> {
        self.function.walk()
    }

    fn view(&self) -> Option<&::dot::View<NodeId>> {
        self.view.as_ref()
    }

    fn dump_as_dot(
        &self,
        node: NodeId,
        out: &mut DotEmitter,
        _state: &mut Self::State,
    ) -> anyhow::Result<()> {
        let dot_id = format!("n{}", node.as_u32());
        out.node(
            &dot_id,
            &self.node_label(node),
            "box",
            self.center_attrs(node),
        );
        // Edge label is producer-output-slot : consumer-input-slot.
        for (in_slot, value) in self.function.node_inputs(node).into_iter().enumerate() {
            let (producer, out_slot) = self.function.value_definition(value);
            if !self.in_view(producer) {
                continue;
            }
            let from = format!("n{}", producer.as_u32());
            let label = format!("{out_slot}:{in_slot}");
            out.edge(&from, &dot_id, &[("label", &label)]);
        }
        Ok(())
    }
}

impl Function {
    /// The renderer of the graph **exactly as stored**, in the dark theme.
    pub fn raw_graph_dot(&self) -> GraphDot<impl GraphDotDumper + '_> {
        GraphDot::new(RawFunctionDumper::new(self), DotStyle::dark())
    }

    /// Graphviz DOT of [`Self::raw_graph_dot`].
    ///
    /// # Errors
    ///
    /// Propagates a DOT-emit error from the renderer.
    pub fn raw_dot(&self) -> crate::Result<String> {
        self.raw_graph_dot().as_dot()
    }

    /// Raw counterpart to the pretty explorer view; needs no Sleigh.
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
        count_producers: bool,
    ) -> crate::Result<String> {
        let set = super::neighborhood::neighborhood_nodes(
            self,
            center,
            depth,
            hub_cap,
            max_nodes,
            count_producers,
        );
        let dumper = RawFunctionDumper {
            view: Some(::dot::View::new(set, center)),
            ..RawFunctionDumper::new(self)
        };
        GraphDot::new(dumper, DotStyle::dark()).as_dot()
    }
}
