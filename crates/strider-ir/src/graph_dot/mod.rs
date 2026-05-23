use rsleigh::MemReader;
use std::collections::HashMap;

use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId};
use crate::node_signature::{SlotRole, expected_signature};

pub mod label;
mod render;
#[cfg(test)]
mod tests;

// ── node appearance ───────────────────────────────────────────────────────────

pub(super) fn node_shape(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::FunctionArg { .. } => "Mdiamond",

        NodeKind::ControlState => "invhouse",
        NodeKind::Phi | NodeKind::MemPhi => "house",

        NodeKind::If => "diamond",

        NodeKind::Load(_)
        | NodeKind::Store(_)
        | NodeKind::StackStore { .. }
        | NodeKind::StackStorePhi { .. } => "box3d",

        NodeKind::Call => "rarrow",
        NodeKind::CallOther { .. } => "doubleoctagon",
        NodeKind::SegmentOp { .. } => "parallelogram",
        NodeKind::CPoolRef => "folder",
        NodeKind::New => "component",

        NodeKind::Return | NodeKind::IndirectBranch => "doublecircle",

        NodeKind::IntConst(_) | NodeKind::BoolConst(_) | NodeKind::FloatConst(_) => "ellipse",

        _ => "box",
    }
}

/// Per-kind fill color for the dark theme.
pub(super) fn node_fillcolor(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::FunctionArg { .. } => "\"#1a3a5c\"",

        NodeKind::ControlState => "\"#2a1a4a\"",

        NodeKind::Phi | NodeKind::MemPhi => "\"#163030\"",

        NodeKind::If => "\"#3a2a10\"",

        NodeKind::Load(_) | NodeKind::Store(_) => "\"#102030\"",

        NodeKind::StackStore { .. } | NodeKind::StackStorePhi { .. } => "\"#20182a\"", // stack-slot purple

        NodeKind::Call => "\"#3a1010\"",
        NodeKind::CallOther { .. } => "\"#3a2810\"", // amber — opaque intrinsic
        NodeKind::SegmentOp { .. } => "\"#10283a\"", // teal — address computation
        NodeKind::CPoolRef => "\"#2a1a3a\"",         // violet — JVM metadata
        NodeKind::New => "\"#103a2a\"",              // dark green — allocation

        NodeKind::Return | NodeKind::IndirectBranch => "\"#103a10\"",

        NodeKind::FloatConst(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatUnaryOp(_)
        | NodeKind::FloatCmpOp(_) => "\"#1a3020\"", // dark green

        NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt
        | NodeKind::CastToFloat => "\"#302018\"", // dark amber

        _ => "\"#2d2d2d\"",
    }
}

// ── edge appearance ───────────────────────────────────────────────────────────

/// Color for a slot role on edge labels.
fn role_color(role: SlotRole) -> &'static str {
    match role {
        SlotRole::Control => "\"#00cccc\"", // aqua
        SlotRole::Memory => "\"#cc88aa\"",  // pink
        SlotRole::Phi | SlotRole::In => "\"#dddddd\"", // white
        SlotRole::Lhs => "\"#4488ff\"",     // blue
        SlotRole::Rhs => "\"#ff4444\"",     // red
        SlotRole::Val | SlotRole::Ret => "\"#88cc88\"", // green
        SlotRole::Addr | SlotRole::Sp | SlotRole::Off => "\"#cc88ff\"", // purple
        SlotRole::Data | SlotRole::Arg | SlotRole::Ref => "\"#ff8800\"", // orange
        SlotRole::Target | SlotRole::Seg => "\"#ffdd44\"", // yellow
        SlotRole::Cond => "\"#ff44ff\"",    // magenta
    }
}

/// Returns `(label, color)` for the edge that delivers `output` as the
/// `input_idx`-th input of `consumer`.  Labels and colours are driven by
/// the consumer's [`Signature`]: the slot's `name` is the label and the
/// slot's `role` selects the colour via [`role_color`].
pub(super) fn edge_style<R: MemReader>(
    dumper: &GraphDotDumper<'_, R>,
    consumer: NodeId,
    input_idx: usize,
    _output: NodeOutputId,
) -> (&'static str, &'static str) {
    let kind = dumper.graph.node_kind(consumer);
    let sig = expected_signature(kind);
    match sig.inputs.at(input_idx) {
        Some(slot) => (slot.name, role_color(slot.role)),
        None => ("", "\"#cccccc\""),
    }
}

// ── dumper ────────────────────────────────────────────────────────────────────

pub struct GraphDotDumper<'a, R: MemReader> {
    pub(crate) entry: NodeId,
    pub(crate) graph: &'a Graph,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    pub(crate) call_clobbered: &'a [rsleigh::Vn],
    /// Calling convention's return-value registers in ABI order.  Used to
    /// label `Return` input edges at slots 2.. with the register name so
    /// visualising the graph shows which vn each return slot carries.
    pub(crate) ret_val_regs: &'a [rsleigh::Vn],
    /// Optional node-id filter.  When `Some(set)`, [`Self::iter_nodes`]
    /// yields only nodes in `set` AND the per-node edge emitter skips
    /// edges whose producer is not in `set`.  Used by per-region /
    /// neighborhood dumps that want to render a subgraph rather than
    /// the whole reachable graph.
    pub(crate) node_filter: Option<crate::walk::NodeIdSet>,
}

impl<'a, R: MemReader> GraphDotDumper<'a, R> {
    /// Returns a copy of this dumper with `node_filter = Some(filter)`.
    /// See the field doc for the filtering contract.
    #[must_use]
    pub fn with_node_filter(mut self, filter: crate::walk::NodeIdSet) -> Self {
        self.node_filter = Some(filter);
        self
    }

    /// Returns `true` when `node` is in the active filter (or there is
    /// no filter, i.e. every node is visible).
    pub(crate) fn is_visible(&self, node: NodeId) -> bool {
        self.node_filter.as_ref().is_none_or(|f| f.contains(node))
    }
}

pub struct GraphDotDumperState {
    pub(super) visited_node_id: HashMap<NodeId, String>,
    /// Synthetic (virtual) DOT nodes inserted between a producer output and
    /// its consumers.  Keyed by the `NodeOutputId` they represent.
    pub(super) virtual_nodes: HashMap<NodeOutputId, String>,
    pub(super) next_unique_id: u32,
}

impl GraphDotDumperState {
    fn alloc_id(&mut self, node_id: NodeId) -> String {
        let id = self.next_unique_id;
        let s = id.to_string();
        self.visited_node_id.insert(node_id, s.clone());
        self.next_unique_id += 1;
        s
    }

    /// Allocates a fresh DOT node id that is NOT associated with any graph
    /// `NodeId` (used for virtual / synthetic nodes).
    pub(super) fn alloc_virtual_id(&mut self) -> String {
        let id = self.next_unique_id;
        self.next_unique_id += 1;
        format!("v{id}")
    }

    pub(super) fn get_dot_id(&mut self, graph: &Graph, node_id: NodeId) -> String {
        // Constants are always given a fresh id so they render as separate nodes.
        if graph.node_kind(node_id).is_const() {
            return self.alloc_id(node_id);
        }
        if let Some(s) = self.visited_node_id.get(&node_id) {
            return s.clone();
        }
        self.alloc_id(node_id)
    }
}
