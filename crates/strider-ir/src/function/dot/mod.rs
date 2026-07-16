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

// ── node appearance ───────────────────────────────────────────────────────────

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

/// Per-kind fill color for the dark theme.
pub(super) fn node_fillcolor(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) => "\"#1a3a5c\"",

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

/// Returns `(label, color)` for the edge that delivers `value` as the
/// `input_idx`-th input of `consumer`.  Labels and colours are driven by
/// the consumer's [`Signature`]: the slot's `name` is the label and the
/// slot's `role` selects the colour via [`role_color`].
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

// ── dumper ────────────────────────────────────────────────────────────────────

pub struct FunctionDotDumper<'a, R: MemReader> {
    pub(crate) entry: NodeId,
    /// Function overlay (including structural graph via `Deref`).
    /// Provides access to both structural graph data and overlay tables
    /// (asm fingerprints, call-other names, stack-phi offsets, phi var tags).
    pub(crate) function: &'a Function,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    /// Reverse map from a carrier `NodeId` to every argument index that
    /// `Function::arg_index_to_values` maps to it (the carrier node recovered
    /// from each value via `Graph::producer`).  Built once at render time from
    /// `function.side_tables().iter_arg_indices()` so per-node label / visual rendering is
    /// O(1).  Empty when `FunctionArgDetect` has not yet run (the underlying
    /// `Function::arg_index_to_values` table is empty).
    pub(crate) node_to_arg_indices: FxHashMap<NodeId, Vec<u32>>,
    /// Restrict the render to these nodes; `None` renders everything reachable
    /// from `entry`.  An edge whose producer falls outside the set is dropped,
    /// so the result is the induced subgraph.  This is what makes the
    /// neighbourhood view the SAME renderer as the full view rather than a
    /// parallel one (see [`FunctionDotDumper::neighborhood_dot`]).
    pub(crate) nodes: Option<FxHashSet<NodeId>>,
    /// Draw this node with a highlight border — the focus of a neighbourhood
    /// render.  `None` for a full render.
    pub(crate) center: Option<NodeId>,
}

/// Build the `node_to_arg_indices` reverse map from `function.side_tables().iter_arg_indices()`.
/// Called once at construction time inside [`Function::dot_dumper`] and in
/// test helpers that construct a [`FunctionDotDumper`] directly.
pub(crate) fn build_arg_reverse_map(function: &Function) -> FxHashMap<NodeId, Vec<u32>> {
    let mut map: FxHashMap<NodeId, Vec<u32>> = FxHashMap::default();
    for idx in function.side_tables().iter_arg_indices() {
        for &value in function.side_tables().arg_index_to_values(idx) {
            let node = function.graph().producer(value);
            map.entry(node).or_default().push(idx);
        }
    }
    // Sort each Vec so label output is deterministic.
    for v in map.values_mut() {
        v.sort_unstable();
    }
    map
}

pub struct FunctionDotDumperState {
    /// Synthetic (virtual) DOT nodes inserted between a producer output and
    /// its consumers.  Keyed by the `ValueId` they represent.
    pub(super) virtual_nodes: FxHashMap<ValueId, String>,
    /// Every emitted DOT id that stands for an IR node, mapped back to it.
    ///
    /// Many-to-one, and that is the point: a constant renders as a fresh box
    /// per use so a hot `0` never becomes an edge hub, and every one of those
    /// boxes maps to the same `NodeId`.  Total over NodeId-backed nodes by
    /// construction — [`get_dot_id`](Self::get_dot_id) is the only way such an
    /// id is minted, and it is the only writer here.
    ///
    /// Virtual nodes are deliberately ABSENT: an `If`'s `if.true` box or a
    /// `Call`'s clobber box is not an IR node and has no `NodeId`, so a
    /// reverse lookup on one yields `None`.
    pub(super) dot_to_node: FxHashMap<String, NodeId>,
    pub(super) next_unique_id: u32,
    /// The neighbourhood centre, mirrored from [`FunctionDotDumper::center`]
    /// so [`get_dot_id`](Self::get_dot_id) can keep it addressable.  `None`
    /// for a full render.
    pub(super) center: Option<NodeId>,
}

impl FunctionDotDumperState {
    /// The IR node a rendered DOT id stands for, or `None` for a virtual
    /// (`If` branch / `Call` clobber) box, which has no `NodeId`.
    pub fn node_of_dot_id(&self, dot_id: &str) -> Option<NodeId> {
        self.dot_to_node.get(dot_id).copied()
    }

    /// Every `(dot id, node)` pair emitted, for callers that want the whole
    /// mapping (the explorer) rather than a point lookup.
    pub fn dot_to_node(&self) -> impl Iterator<Item = (&str, NodeId)> {
        self.dot_to_node.iter().map(|(k, &v)| (k.as_str(), v))
    }

    /// Allocates a fresh DOT node id that is NOT associated with any graph
    /// `NodeId` (used for virtual / synthetic nodes).  Intentionally absent
    /// from [`dot_to_node`](Self::dot_to_node) — see its docs.
    pub(super) fn alloc_virtual_id(&mut self) -> String {
        let id = self.next_unique_id;
        self.next_unique_id += 1;
        format!("v{id}")
    }

    /// Whether `node` draws a private box beside each of its consumers instead
    /// of one shared box the whole graph points at.
    ///
    /// True for a constant: a hot `0` used in fifty places would otherwise be a
    /// fifty-edge hub that drags the layout into a hairball.  The neighbourhood
    /// centre is the one exception — it is what the explorer re-centres and
    /// searches on, so it stays a single addressable box even when const.
    pub(super) fn renders_per_use(&self, graph: &Graph, node: NodeId) -> bool {
        graph.node_kind(node).is_const() && self.center != Some(node)
    }

    /// The DOT id for `node`.
    ///
    /// A real node's id IS its `NodeId`, which makes it directly addressable
    /// (the explorer navigates by it) and self-memoizing: a node reached from
    /// several edges resolves to the same id, so it renders as one box with no
    /// bookkeeping.  The CFG dumper already works this way; this is the IR
    /// side agreeing with it.
    ///
    /// A [per-use](Self::renders_per_use) constant is the exception: since it
    /// draws a fresh box at each consumer, its id must be unique per use.  Those
    /// get a `c`-prefixed counter — not a navigation target, but still mapped
    /// back to the node in [`dot_to_node`](Self::dot_to_node).  The `c` / `v`
    /// prefixes keep both off the integer id space.
    ///
    /// `graph` is used for the node-kind lookup only; callers pass
    /// `dumper.function.graph()` or a deref of the function.
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
