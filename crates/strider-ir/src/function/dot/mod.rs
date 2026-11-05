use ::dot::GraphDotDumper as _;
use rustc_hash::FxHashMap;

use crate::IRViewer;
use crate::function::Function;
use crate::node::signature::{SlotRole, expected_signature};
use crate::node::{NodeId, NodeKind, ValueId};

pub(crate) mod label;
mod neighborhood;
mod raw;
mod render;
#[cfg(test)]
mod tests;

/// Graphviz shape and dark-theme fill color.
pub(super) fn node_style(kind: &NodeKind) -> (&'static str, &'static str) {
    match kind {
        NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) => {
            ("Mdiamond", "#1a3a5c")
        }
        NodeKind::Region => ("invhouse", "#2a1a4a"),
        NodeKind::Phi | NodeKind::MemPhi => ("house", "#163030"),
        NodeKind::If => ("diamond", "#3a2a10"),
        NodeKind::Load(_) | NodeKind::Store(_) => ("box3d", "#102030"),
        NodeKind::Call { .. } => ("rarrow", "#3a1010"),
        NodeKind::CallOther { .. } => ("doubleoctagon", "#3a2810"), // amber: opaque intrinsic
        NodeKind::CPoolRef => ("folder", "#2a1a3a"),                // violet: JVM metadata
        NodeKind::New => ("component", "#103a2a"),                  // dark green: allocation
        NodeKind::Return | NodeKind::IndirectBranch => ("doublecircle", "#103a10"),
        NodeKind::Switch(_) => ("trapezium", "#103a10"),
        NodeKind::Unreachable => ("Msquare", "#3a1030"), // magenta: a sink control cannot leave
        NodeKind::IntConst(_) => ("ellipse", "#2d2d2d"),
        NodeKind::FloatConst(_) => ("ellipse", "#1a3020"),
        NodeKind::IntUnaryOp(_)
        | NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::Truncate
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::Extend(_) => ("box", "#2d2d2d"),
        NodeKind::FloatBinaryOp(_) | NodeKind::FloatUnaryOp(_) | NodeKind::FloatCmpOp(_) => {
            ("box", "#1a3020") // dark green
        }
        NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt => ("box", "#302018"), // dark amber
    }
}

pub(super) fn role_color(role: SlotRole) -> &'static str {
    match role {
        SlotRole::Control => "#00cccc",                              // aqua
        SlotRole::Memory => "#cc88aa",                               // pink
        SlotRole::Phi | SlotRole::In => "#dddddd",                   // white
        SlotRole::Lhs => "#4488ff",                                  // blue
        SlotRole::Rhs => "#ff4444",                                  // red
        SlotRole::Val | SlotRole::Ret => "#88cc88",                  // green
        SlotRole::Addr => "#cc88ff",                                 // purple
        SlotRole::Data | SlotRole::Arg | SlotRole::Ref => "#ff8800", // orange
        SlotRole::Target | SlotRole::Sp => "#ffdd44",                // yellow
        SlotRole::Cond => "#ff44ff",                                 // magenta
    }
}

/// `(label, ordinal within a repeating tail, color)` of the edge into slot
/// `input_idx` of `consumer`, from its expected signature.
pub(super) fn edge_style(
    consumer: &NodeKind,
    input_idx: usize,
) -> (&'static str, Option<usize>, &'static str) {
    let sig = expected_signature(consumer);
    match sig.inputs.at_indexed(input_idx) {
        Some((slot, tail_idx)) => (slot.name, tail_idx, role_color(slot.role)),
        None => ("", None, "#cccccc"),
    }
}

impl Function {
    /// The pretty renderer, naming registers and spaces through `names`.
    pub fn dot_dumper<'a>(&'a self, names: &'a strider_target::ArchNames) -> FunctionDotDumper<'a> {
        FunctionDotDumper {
            function: self,
            names,
            arg_labels: arg_labels(self),
            view: None,
        }
    }
}

pub struct FunctionDotDumper<'a> {
    function: &'a Function,
    /// The arch's register and space names, all a label reads.
    names: &'a strider_target::ArchNames,
    /// Carrier node -> its arg tags, joined.
    arg_labels: FxHashMap<NodeId, String>,
    /// A neighbourhood render; `None` renders everything reachable from entry.
    view: Option<::dot::View<NodeId>>,
}

/// A carrier's argument annotation. The integer and float index spaces are
/// separate, so the class is part of the label.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ArgTag {
    Int(u32),
    Float(u32),
}

impl std::fmt::Display for ArgTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(index) => write!(f, "[arg {index}]"),
            Self::Float(index) => write!(f, "[float arg {index}]"),
        }
    }
}

/// Carrier node -> its arg tags, sorted and `", "`-joined.
fn arg_labels(function: &Function) -> FxHashMap<NodeId, String> {
    let mut map: FxHashMap<NodeId, Vec<ArgTag>> = FxHashMap::default();
    let side = function.side_tables();
    let tags = side
        .iter_arg_indices()
        .map(|index| (ArgTag::Int(index), side.arg_index_to_values(index)))
        .chain(
            side.iter_float_arg_indices()
                .map(|index| (ArgTag::Float(index), side.float_arg_index_to_values(index))),
        );
    for (tag, values) in tags {
        for &value in values {
            let node = function.producer(value);
            map.entry(node).or_default().push(tag);
        }
    }
    map.into_iter()
        .map(|(node, mut tags)| {
            tags.sort_unstable();
            let joined: Vec<String> = tags.iter().map(ToString::to_string).collect();
            (node, joined.join(", "))
        })
        .collect()
}

#[derive(Default)]
pub struct FunctionDotDumperState {
    /// Virtual DOT nodes inserted between a producer output and its consumers,
    /// keyed by the `ValueId` they stand for.
    pub(super) virtual_nodes: FxHashMap<ValueId, String>,
    /// Every emitted DOT id that stands for an IR node, mapped back to it.
    /// Many-to-one: a per-use constant's boxes all map to the same `NodeId`.
    #[cfg(test)]
    pub(super) dot_to_node: FxHashMap<String, NodeId>,
    pub(super) next_unique_id: u32,
}

impl FunctionDotDumperState {
    /// `None` for a virtual (`If` branch / `Call` clobber) box.
    #[cfg(test)]
    pub fn node_of_dot_id(&self, dot_id: &str) -> Option<NodeId> {
        self.dot_to_node.get(dot_id).copied()
    }

    /// The full emitted-id -> `NodeId` mapping.
    #[cfg(test)]
    pub fn dot_to_node(&self) -> impl Iterator<Item = (&str, NodeId)> {
        self.dot_to_node.iter().map(|(k, &v)| (k.as_str(), v))
    }

    fn fresh_id(&mut self, prefix: char) -> String {
        let id = self.next_unique_id;
        self.next_unique_id += 1;
        format!("{prefix}{id}")
    }

    /// A DOT id backed by no graph `NodeId`, for virtual nodes.
    pub(super) fn alloc_virtual_id(&mut self) -> String {
        self.fresh_id('v')
    }
}

impl FunctionDotDumper<'_> {
    /// Whether `node` draws a private box beside each consumer instead of one
    /// shared box. True for constants, except the neighbourhood centre.
    fn renders_per_use(&self, node: NodeId) -> bool {
        self.function.node_kind(node).is_const() && !self.is_center(node)
    }

    /// The DOT id for `node`: its bare `NodeId`, or a fresh `c`-prefixed
    /// counter for a [per-use](Self::renders_per_use) constant.
    fn dot_id(&self, state: &mut FunctionDotDumperState, node_id: NodeId) -> String {
        let s = if self.renders_per_use(node_id) {
            state.fresh_id('c')
        } else {
            node_id.as_u32().to_string()
        };
        #[cfg(test)]
        state.dot_to_node.insert(s.clone(), node_id);
        s
    }
}
