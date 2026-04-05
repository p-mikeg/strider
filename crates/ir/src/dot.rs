use std::collections::HashMap;
use rsleigh::MemReader;

use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};


// ── node appearance ───────────────────────────────────────────────────────────

fn node_shape(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)       => "Mdiamond",

        NodeKind::ControlState          => "invhouse",
        NodeKind::ControlSelector(_)
        | NodeKind::MemSelector         => "house",

        NodeKind::If                    => "diamond",
        NodeKind::IfCase(_)             => "trapezium",

        NodeKind::Load(_)
        | NodeKind::Store(_)            => "box3d",

        NodeKind::Call                  => "rarrow",

        NodeKind::PostCallMemState
        | NodeKind::PostCallVarState(_) => "invtriangle",

        NodeKind::Return                => "doublecircle",

        NodeKind::IntConst(_)
        | NodeKind::BoolConst(_)        => "ellipse",

        _ => "box",
    }
}

/// Per-kind fill color for the dark theme.
fn node_fillcolor(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)       => "\"#1a3a5c\"",

        NodeKind::ControlState          => "\"#2a1a4a\"",

        NodeKind::ControlSelector(_)
        | NodeKind::MemSelector         => "\"#163030\"",

        NodeKind::If
        | NodeKind::IfCase(_)           => "\"#3a2a10\"",

        NodeKind::Load(_)
        | NodeKind::Store(_)            => "\"#102030\"",

        NodeKind::Call                  => "\"#3a1010\"",

        NodeKind::PostCallMemState
        | NodeKind::PostCallVarState(_) => "\"#28102a\"",

        NodeKind::Return                => "\"#103a10\"",

        _ => "\"#2d2d2d\"",
    }
}


// ── edge appearance ───────────────────────────────────────────────────────────

/// Returns `(label, color)` for the edge that delivers `output` as the
/// `input_idx`-th input of `consumer`.
fn edge_style<R: MemReader>(
    dumper: &GraphDotDumper<'_, R>,
    consumer: NodeId,
    input_idx: usize,
    output: NodeOutputId,
) -> (&'static str, &'static str) {
    let out_kind = dumper.graph.output_kind(output);

    // Non-value output kinds always use the same role regardless of position.
    match out_kind {
        NodeOutputKind::Control        => return ("ctrl",     "\"#00cccc\""),   // aqua
        NodeOutputKind::Memory         => return ("mem",      "\"#cc88aa\""),   // pink
        NodeOutputKind::ControlSelector => return ("sel",     "\"#dddddd\""),   // white
        NodeOutputKind::OutputType(_)  => {}                                     // fall through
    }

    // Value edges: colour/label depend on how the consumer uses this slot.
    match dumper.graph.node_kind(consumer) {
        NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::BoolBinaryOp(_) => match input_idx {
            0 => ("lhs", "\"#4488ff\""),   // blue
            1 => ("rhs", "\"#ff4444\""),   // red
            _ => ("",    "\"#cccccc\""),
        },

        NodeKind::IntUnaryOp(_)
        | NodeKind::BoolUnaryOp(_)
        | NodeKind::Extend(_)
        | NodeKind::CastToBool
        | NodeKind::CastToInt
        | NodeKind::Truncate
        | NodeKind::Popcount  => ("val", "\"#88cc88\""),   // green

        NodeKind::Load(_) => match input_idx {
            0 => ("mem",  "\"#cc88aa\""),
            1 => ("addr", "\"#cc88ff\""),  // purple
            _ => ("",     "\"#cccccc\""),
        },

        NodeKind::Store(_) => match input_idx {
            0 => ("mem",  "\"#cc88aa\""),
            1 => ("addr", "\"#cc88ff\""),  // purple
            2 => ("data", "\"#ff8800\""),  // orange
            _ => ("",     "\"#cccccc\""),
        },

        NodeKind::Call => match input_idx {
            0 => ("ctrl",   "\"#00cccc\""),
            1 => ("mem",    "\"#cc88aa\""),
            2 => ("target", "\"#ffdd44\""),  // yellow
            _ => ("arg",    "\"#ff8800\""),  // orange
        },

        NodeKind::If => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("cond", "\"#ff44ff\""),  // magenta
            _ => ("",     "\"#cccccc\""),
        },

        NodeKind::Return => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("mem",  "\"#cc88aa\""),
            2 => ("val",  "\"#88cc88\""),
            _ => ("",     "\"#cccccc\""),
        },

        NodeKind::ControlState => ("ctrl", "\"#00cccc\""),

        NodeKind::ControlSelector(_)
        | NodeKind::MemSelector => ("in", "\"#dddddd\""),

        NodeKind::PostCallMemState
        | NodeKind::PostCallVarState(_) => match input_idx {
            0 => ("ctrl",  "\"#00cccc\""),
            1 => ("mem",   "\"#cc88aa\""),
            _ => ("",      "\"#cccccc\""),
        },

        _ => ("", "\"#cccccc\""),
    }
}


// ── dumper ────────────────────────────────────────────────────────────────────

pub struct GraphDotDumper<'a, R: MemReader> {
    pub(crate) entry: NodeId,
    pub(crate) graph: &'a Graph,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    pub(crate) call_clobbered: &'a HashMap<crate::node::NodeId, Box<[rsleigh::Vn]>>,
}

impl<'a, R: MemReader> GraphDotDumper<'a, R> {
    fn vn_to_name(&self, vn: &rsleigh::Vn) -> String {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST    => format!("{offset:#x}:{size}"),
            rsleigh::VnSpace::REGISTER => {
                let regs = self.sleigh.regs().unwrap();
                regs.vn_to_name(*vn).unwrap().to_string()
            },
            rsleigh::VnSpace::RAM      => format!("ram[{offset:#x}]:{size}"),
            rsleigh::VnSpace::UNIQUE   => format!("unique[{offset:#x}]:{size}"),
            s if s == self.sleigh.default_code_space() => format!("ram[{offset:#x}]:{size}"),
            _                          => unreachable!(),
        }
    }

    /// Returns the display name for a VnSpace, checking the default code space
    /// so that architectures where the code space isn't literally `RAM` still
    /// render as "ram".
    fn pretty_vnspace(&self, space: rsleigh::VnSpace) -> &'static str {
        let default = self.sleigh.default_code_space();
        if space == default {
            return "ram";
        }
        match space {
            rsleigh::VnSpace::CONST    => "const",
            rsleigh::VnSpace::REGISTER => "register",
            rsleigh::VnSpace::UNIQUE   => "unique",
            rsleigh::VnSpace::RAM      => "ram",
            _                          => "??",
        }
    }

    /// Human-readable string for a `NodeKind` (dot-only).
    fn node_kind_str(&self, kind: &NodeKind) -> String {
        match kind {
            NodeKind::CastToBool | NodeKind::CastToInt => "Cast".to_owned(),
            NodeKind::Truncate                         => "Truncate".to_owned(),
            NodeKind::Extend(op)                       => format!("{:?}", op),
            NodeKind::BoolConst(v)                     => format!("const {v}"),
            NodeKind::IntConst(v)                      => format!("const {v:#x}"),
            NodeKind::BoolBinaryOp(op)                 => format!("{:?}", op),
            NodeKind::IntBinaryOp(op)                  => format!("{:?}", op),
            NodeKind::BoolUnaryOp(op)                  => format!("{:?}", op),
            NodeKind::IntUnaryOp(op)                   => format!("{:?}", op),
            NodeKind::IntCmpOp(op)                     => format!("{:?}", op),
            NodeKind::Load(space)                      => format!("Load {}", self.pretty_vnspace(*space)),
            NodeKind::Store(space)                     => format!("Store {}", self.pretty_vnspace(*space)),
            _                                          => format!("{:?}", kind),
        }
    }

    fn pretty_label(&self, node: NodeId) -> String {
        let kind = self.graph.node_kind(node);
        let base = match kind {
            NodeKind::InitialVar(var)       => format!("init\n{}", self.vn_to_name(var)),
            NodeKind::ControlSelector(var)  => format!("φ {}", self.vn_to_name(var)),
            NodeKind::PostCallVarState(var) => format!("post-call\n{}", self.vn_to_name(var)),
            NodeKind::IfCase(b)             => format!("if.{}", if *b { "true" } else { "false" }),
            _                               => self.node_kind_str(kind),
        };

        // Append output type for bool-producing and unary/cast operations so
        // the type transformation is visible in the graph.
        let show_type = matches!(kind,
            NodeKind::IntUnaryOp(_)
            | NodeKind::BoolUnaryOp(_)
            | NodeKind::CastToBool
            | NodeKind::CastToInt
            | NodeKind::Extend(_)
            | NodeKind::Truncate
            | NodeKind::Popcount
            | NodeKind::IntCmpOp(_)
            | NodeKind::BoolBinaryOp(_)
        );

        if show_type {
            if let Some(ty) = self.graph.node_outputs(node).into_iter()
                .find_map(|o| self.graph.output_kind(o).as_value())
            {
                return format!("{}\n:{}", base, ty.as_str());
            }
        }

        base
    }

    fn emit_const_node(&self, node: NodeId, dot_id: &str, out: &mut dot::DotEmitter) {
        let kind = self.graph.node_kind(node);
        assert!(kind.is_const());
        let fc = node_fillcolor(&kind);
        out.node(dot_id, &self.node_kind_str(kind), "ellipse", &[("fillcolor", fc)]);
    }

    /// Returns the register name for a clobbered call output using the
    /// `call_clobbered` map: the i-th clobbered output (output_index - 2)
    /// corresponds to the i-th vn in the map entry for that call node.
    fn call_clobbered_name(&self, output_id: NodeOutputId) -> String {
        let (call_id, output_index) = self.graph.output_definition(output_id);
        let i = (output_index - 2) as usize;
        let vn = &self.call_clobbered[&call_id][i];
        self.vn_to_name(vn)
    }

}

pub struct GraphDotDumperState {
    visited_node_id: HashMap<NodeId, String>,
    /// Synthetic (virtual) DOT nodes inserted between a producer output and
    /// its consumers.  Keyed by the `NodeOutputId` they represent.
    virtual_nodes: HashMap<NodeOutputId, String>,
    next_unique_id: u32,
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
    fn alloc_virtual_id(&mut self) -> String {
        let id = self.next_unique_id;
        self.next_unique_id += 1;
        format!("v{id}")
    }

    fn get_dot_id(&mut self, graph: &Graph, node_id: NodeId) -> String {
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

impl<'a, R: MemReader> dot::GraphDotDumper for GraphDotDumper<'a, R> {
    type Node  = NodeId;
    type Error = std::io::Error;
    type State = GraphDotDumperState;

    fn create_initial_state(&self) -> Self::State {
        Self::State {
            visited_node_id: HashMap::new(),
            virtual_nodes: HashMap::new(),
            next_unique_id: 0,
        }
    }

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        crate::walk::walk_graph(self.graph, self.entry)
    }

    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut dot::DotEmitter,
        state: &mut Self::State,
    ) -> core::result::Result<(), Self::Error> {
        if self.graph.node_kind(node).is_const() {
            return Ok(());
        }

        let kind = self.graph.node_kind(node);
        let cur_id = state.get_dot_id(self.graph, node);
        let shape  = node_shape(&kind);
        let fc     = node_fillcolor(&kind);

        out.node(&cur_id, &self.pretty_label(node), shape, &[("fillcolor", fc)]);

        // ── Virtual nodes for structured outputs ──────────────────────────────

        match kind {
            // For the two control outputs of an If node, emit "if.true" and
            // "if.false" virtual nodes so each branch is clearly labelled.
            NodeKind::If => {
                let outputs = self.graph.node_outputs(node);
                let branch_labels = ["if.true", "if.false"];
                let edge_labels   = ["true",    "false"];
                for ((out_id, blabel), elabel) in outputs.into_iter()
                    .zip(branch_labels.iter())
                    .zip(edge_labels.iter())
                {
                    let virt_id = state.alloc_virtual_id();
                    out.node(&virt_id, blabel, "trapezium", &[
                        ("fillcolor", "\"#3a2a10\""),
                    ]);
                    out.edge(&cur_id, &virt_id, &[
                        ("color",     "\"#00cccc\""),
                        ("label",     elabel),
                        ("fontcolor", "\"#cccccc\""),
                        ("fontsize",  "9"),
                    ]);
                    state.virtual_nodes.insert(out_id, virt_id);
                }
            }

            _ => {}
        }

        // ── Draw edges from this node's inputs to this node ───────────────────

        for (idx, parent_output) in self.graph.node_inputs(node).into_iter().enumerate() {
            let parent_id = self.graph.get_node_from_output(parent_output);

            // If the producing output has a virtual node, connect from it.
            // For clobbered Call outputs (index >= 2), create the virtual node
            // on the fly the first time a consumer is encountered.
            let parent_dot_id = {
                let maybe_virt = state.virtual_nodes.get(&parent_output).cloned();
                if let Some(virt_id) = maybe_virt {
                    virt_id
                } else if *self.graph.node_kind(parent_id) == NodeKind::Call {
                    let (_, output_index) = self.graph.output_definition(parent_output);
                    if output_index >= 2 {
                        let name = self.call_clobbered_name(parent_output);
                        let label = format!("Post Call\n{name}");
                        let virt_id = state.alloc_virtual_id();
                        let call_dot_id = state.get_dot_id(self.graph, parent_id);
                        out.node(&virt_id, &label, "box", &[
                            ("fillcolor", "\"#28102a\""),
                            ("style",     "\"filled,dashed\""),
                        ]);
                        out.edge(&call_dot_id, &virt_id, &[
                            ("color", "\"#888888\""),
                            ("style", "dashed"),
                        ]);
                        state.virtual_nodes.insert(parent_output, virt_id.clone());
                        virt_id
                    } else {
                        state.get_dot_id(self.graph, parent_id)
                    }
                } else {
                    state.get_dot_id(self.graph, parent_id)
                }
            };

            let (label, color) = edge_style(self, node, idx, parent_output);

            let mut extra: Vec<(&str, &str)> = vec![("color", color)];
            if !label.is_empty() {
                extra.push(("label", label));
                extra.push(("fontcolor", "\"#cccccc\""));
                extra.push(("fontsize", "9"));
            }

            out.edge(&parent_dot_id, &cur_id, &extra);

            if self.graph.node_kind(parent_id).is_const() {
                self.emit_const_node(parent_id, &parent_dot_id, out);
            }
        }

        Ok(())
    }
}
