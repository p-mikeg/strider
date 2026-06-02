use rsleigh::MemReader;
use rustc_hash::FxHashMap;

use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueId};
use crate::node_signature::{SlotRole, expected_signature};

pub mod label;
mod raw;
mod render;
#[cfg(test)]
mod tests;

// ── node appearance ───────────────────────────────────────────────────────────

pub(super) fn node_shape(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_) => "Mdiamond",

        NodeKind::Region => "invhouse",
        NodeKind::Phi | NodeKind::MemPhi => "house",

        NodeKind::If => "diamond",

        NodeKind::Load(_)
        | NodeKind::Store(_) => "box3d",

        NodeKind::Call => "rarrow",
        NodeKind::CallOther { .. } => "doubleoctagon",
        NodeKind::SegmentOp { .. } => "parallelogram",
        NodeKind::CPoolRef => "folder",
        NodeKind::New => "component",

        NodeKind::Return | NodeKind::IndirectBranch => "doublecircle",

        NodeKind::IntConst(_) | NodeKind::FloatConst(_) => "ellipse",

        _ => "box",
    }
}

/// Per-kind fill color for the dark theme.
pub(super) fn node_fillcolor(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_) => "\"#1a3a5c\"",

        NodeKind::Region => "\"#2a1a4a\"",

        NodeKind::Phi | NodeKind::MemPhi => "\"#163030\"",

        NodeKind::If => "\"#3a2a10\"",

        NodeKind::Load(_) | NodeKind::Store(_) => "\"#102030\"",

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
        | NodeKind::FloatBitsToInt => "\"#302018\"", // dark amber

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
        SlotRole::Addr | SlotRole::Off => "\"#cc88ff\"", // purple
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
    dumper: &FunctionDotDumper<'_, R>,
    consumer: NodeId,
    input_idx: usize,
    _output: ValueId,
) -> (&'static str, &'static str) {
    let kind = dumper.function.node_kind(consumer);
    let sig = expected_signature(kind);
    match sig.inputs.at(input_idx) {
        Some(slot) => (slot.name, role_color(slot.role)),
        None => ("", "\"#cccccc\""),
    }
}

// ── dumper ────────────────────────────────────────────────────────────────────

pub struct FunctionDotDumper<'a, R: MemReader> {
    pub(crate) entry: NodeId,
    /// Function overlay (including structural graph via `Deref`).
    /// Provides access to both structural graph data and overlay tables
    /// (asm fingerprints, call-other names, stack-phi offsets, phi var tags).
    pub(crate) function: &'a Function,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    /// Optional node-id filter.  When `Some(set)`, [`Self::iter_nodes`]
    /// yields only nodes in `set` AND the per-node edge emitter skips
    /// edges whose producer is not in `set`.  Used by per-region /
    /// neighborhood dumps that want to render a subgraph rather than
    /// the whole reachable graph.
    pub(crate) node_filter: Option<crate::walk::NodeIdSet>,
    /// Reverse map from a carrier `NodeId` to every argument index that
    /// `Function::arg_index_to_nodes` maps to it.  Built once at render
    /// time from `function.iter_arg_indices()` so per-node label / visual
    /// rendering is O(1).  Empty when `FunctionArgDetect` has not yet run
    /// (the underlying `Function::arg_index_to_nodes` table is empty).
    pub(crate) node_to_arg_indices: FxHashMap<NodeId, Vec<u32>>,
}

/// Build the `node_to_arg_indices` reverse map from `function.iter_arg_indices()`.
/// Called once at construction time inside [`Function::dot_dumper`] and in
/// test helpers that construct a [`FunctionDotDumper`] directly.
pub fn build_arg_reverse_map(function: &Function) -> FxHashMap<NodeId, Vec<u32>> {
    let mut map: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
    for idx in function.iter_arg_indices() {
        for &node in function.arg_index_to_nodes(idx) {
            map.entry(node).or_default().push(idx);
        }
    }
    // Sort each Vec so label output is deterministic.
    for v in map.values_mut() {
        v.sort_unstable();
    }
    map
}

impl<'a, R: MemReader> FunctionDotDumper<'a, R> {
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

pub struct FunctionDotDumperState {
    pub(super) visited_node_id: FxHashMap<NodeId, String>,
    /// Synthetic (virtual) DOT nodes inserted between a producer output and
    /// its consumers.  Keyed by the `ValueId` they represent.
    pub(super) virtual_nodes: FxHashMap<ValueId, String>,
    pub(super) next_unique_id: u32,
}

impl FunctionDotDumperState {
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
        // Note: `graph` here is used for structural node-kind lookup only;
        // callers pass `dumper.function.graph()` or a deref of the function.
        if graph.node_kind(node_id).is_const() {
            return self.alloc_id(node_id);
        }
        if let Some(s) = self.visited_node_id.get(&node_id) {
            return s.clone();
        }
        self.alloc_id(node_id)
    }
}
