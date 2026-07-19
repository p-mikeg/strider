use rsleigh::MemReader;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::IRViewer;
use crate::function::Function;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, ValueId};
use crate::node_signature::{SlotRole, expected_signature};

pub(crate) mod label;
mod neighborhood;
mod raw;
mod render;
#[cfg(test)]
mod tests;

pub(super) fn node_shape(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) => "Mdiamond",

        NodeKind::Region => "invhouse",
        NodeKind::Phi | NodeKind::MemPhi => "house",

        NodeKind::If => "diamond",

        NodeKind::Load(_) | NodeKind::Store(_) => "box3d",

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

/// Dark-theme fill color.
pub(super) fn node_fillcolor(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) => "\"#1a3a5c\"",

        NodeKind::Region => "\"#2a1a4a\"",

        NodeKind::Phi | NodeKind::MemPhi => "\"#163030\"",

        NodeKind::If => "\"#3a2a10\"",

        NodeKind::Load(_) | NodeKind::Store(_) => "\"#102030\"",

        NodeKind::Call => "\"#3a1010\"",
        NodeKind::CallOther { .. } => "\"#3a2810\"", // amber: opaque intrinsic
        NodeKind::SegmentOp { .. } => "\"#10283a\"", // teal: address computation
        NodeKind::CPoolRef => "\"#2a1a3a\"",         // violet: JVM metadata
        NodeKind::New => "\"#103a2a\"",              // dark green: allocation

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

pub(super) fn role_color(role: SlotRole) -> &'static str {
    match role {
        SlotRole::Control => "\"#00cccc\"",              // aqua
        SlotRole::Memory => "\"#cc88aa\"",               // pink
        SlotRole::Phi | SlotRole::In => "\"#dddddd\"",   // white
        SlotRole::Lhs => "\"#4488ff\"",                  // blue
        SlotRole::Rhs => "\"#ff4444\"",                  // red
        SlotRole::Val | SlotRole::Ret => "\"#88cc88\"",  // green
        SlotRole::Addr | SlotRole::Off => "\"#cc88ff\"", // purple
        SlotRole::Data | SlotRole::Arg | SlotRole::Ref => "\"#ff8800\"", // orange
        SlotRole::Target | SlotRole::Seg | SlotRole::Sp => "\"#ffdd44\"", // yellow
        SlotRole::Cond => "\"#ff44ff\"",                 // magenta
    }
}

/// `(label, color)` for the edge delivering `value` into `consumer`'s
/// `input_idx`-th slot, both driven by the consumer's expected signature.
pub(super) fn edge_style<R: MemReader>(
    dumper: &FunctionDotDumper<'_, R>,
    consumer: NodeId,
    input_idx: usize,
    _value: ValueId,
) -> (&'static str, &'static str) {
    let kind = dumper.function.node_kind(consumer);
    let sig = expected_signature(kind);
    match sig.inputs.at(input_idx) {
        Some(slot) => (slot.name, role_color(slot.role)),
        None => ("", "\"#cccccc\""),
    }
}

pub struct FunctionDotDumper<'a, R: MemReader> {
    pub(crate) entry: NodeId,
    pub(crate) function: &'a Function,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    /// Reverse of `Function::arg_index_to_values`: carrier node -> arg indices.
    /// Built once at render time so per-node label lookup is O(1).  Empty until
    /// `FunctionArgDetect` has run.
    pub(crate) node_to_arg_indices: FxHashMap<NodeId, Vec<u32>>,
    /// Restrict the render to these nodes; `None` renders everything reachable
    /// from `entry`.  Edges whose producer falls outside are dropped, so the
    /// result is the induced subgraph.  Lets the neighbourhood view reuse this
    /// renderer instead of being a parallel one.
    pub(crate) nodes: Option<FxHashSet<NodeId>>,
    /// Focus of a neighbourhood render, drawn with a highlight border.
    pub(crate) center: Option<NodeId>,
}

pub(crate) fn build_arg_reverse_map(function: &Function) -> FxHashMap<NodeId, Vec<u32>> {
    let mut map: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
    for idx in function.side_tables().iter_arg_indices() {
        for &value in function.side_tables().arg_index_to_values(idx) {
            let node = function.producer(value);
            map.entry(node).or_default().push(idx);
        }
    }
    // Sort so label output is deterministic.
    for v in map.values_mut() {
        v.sort_unstable();
    }
    map
}

pub struct FunctionDotDumperState {
    /// Virtual DOT nodes inserted between a producer output and its consumers,
    /// keyed by the `ValueId` they stand for.
    pub(super) virtual_nodes: FxHashMap<ValueId, String>,
    /// Every emitted DOT id that stands for an IR node, mapped back to it.
    ///
    /// Many-to-one by design: a constant renders as a fresh box per use, and
    /// each of those boxes maps to the same `NodeId`.  Total over NodeId-backed
    /// nodes, since [`get_dot_id`](Self::get_dot_id) is the only minter and the
    /// only writer.  Virtual nodes are absent (they have no `NodeId`).
    pub(super) dot_to_node: FxHashMap<String, NodeId>,
    pub(super) next_unique_id: u32,
    /// Mirrored from [`FunctionDotDumper::center`] so
    /// [`get_dot_id`](Self::get_dot_id) can keep the centre addressable.
    pub(super) center: Option<NodeId>,
}

impl FunctionDotDumperState {
    /// `None` for a virtual (`If` branch / `Call` clobber) box, which has no
    /// `NodeId`.
    pub fn node_of_dot_id(&self, dot_id: &str) -> Option<NodeId> {
        self.dot_to_node.get(dot_id).copied()
    }

    pub fn dot_to_node(&self) -> impl Iterator<Item = (&str, NodeId)> {
        self.dot_to_node.iter().map(|(k, &v)| (k.as_str(), v))
    }

    /// A DOT id backed by no graph `NodeId`, for virtual nodes.  Deliberately
    /// absent from [`dot_to_node`](Self::dot_to_node).
    pub(super) fn alloc_virtual_id(&mut self) -> String {
        let id = self.next_unique_id;
        self.next_unique_id += 1;
        format!("v{id}")
    }

    /// Whether `node` draws a private box beside each consumer instead of one
    /// shared box.
    ///
    /// True for constants: a hot `0` used fifty times would otherwise be a
    /// fifty-edge hub that drags the layout into a hairball.  The neighbourhood
    /// centre is exempt; the explorer re-centres and searches on it, so it must
    /// stay one addressable box even when const.
    pub(super) fn renders_per_use(&self, graph: &Graph, node: NodeId) -> bool {
        graph.node_kind(node).is_const() && self.center != Some(node)
    }

    /// A real node's id IS its `NodeId`, which makes it addressable (the
    /// explorer navigates by it) and self-memoizing: a node reached from several
    /// edges resolves to the same id, so it renders as one box with no
    /// bookkeeping.
    ///
    /// A [per-use](Self::renders_per_use) constant draws a fresh box at each
    /// consumer, so its id must be unique per use; those get a `c`-prefixed
    /// counter.  The `c` / `v` prefixes keep virtual and per-use ids off the
    /// integer id space.
    ///
    /// `graph` is for the node-kind lookup only.
    pub(super) fn get_dot_id(&mut self, graph: &Graph, node_id: NodeId) -> String {
        let s = if self.renders_per_use(graph, node_id) {
            let id = self.next_unique_id;
            self.next_unique_id += 1;
            format!("c{id}")
        } else {
            node_id.as_u32().to_string()
        };
        self.dot_to_node.insert(s.clone(), node_id);
        s
    }
}
