use rsleigh::MemReader;
use std::collections::HashMap;
use std::io;

use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

/// Formats a signed SP-relative offset so that the sign is always shown
/// (e.g. `0` → ` + 0`, `-4` → ` - 4`, `8` → ` + 8`).  Used by the StackStore
/// / StackStorePhi labels.
fn signed_offset(o: i64) -> String {
    if o < 0 {
        format!(" - {}", -(o as i128))
    } else {
        format!(" + {o}")
    }
}

// ── node appearance ───────────────────────────────────────────────────────────

fn node_shape(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) => "Mdiamond",

        NodeKind::ControlState => "invhouse",
        NodeKind::ControlPhi(_) | NodeKind::MemPhi => "house",

        NodeKind::If => "diamond",
        NodeKind::IfCase(_) => "trapezium",

        NodeKind::Load(_)
        | NodeKind::Store(_)
        | NodeKind::StackStore { .. }
        | NodeKind::StackStorePhi { .. } => "box3d",

        NodeKind::Call => "rarrow",
        NodeKind::CallOther { .. } => "doubleoctagon",
        NodeKind::SegmentOp { .. } => "parallelogram",
        NodeKind::CPoolRef => "folder",
        NodeKind::New => "component",

        NodeKind::PostCallMemState | NodeKind::PostCallVarState(_) => "invtriangle",

        NodeKind::Return => "doublecircle",

        NodeKind::IntConst(_) | NodeKind::BoolConst(_) | NodeKind::FloatConst(_) => "ellipse",

        _ => "box",
    }
}

/// Per-kind fill color for the dark theme.
fn node_fillcolor(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Entry | NodeKind::InitialMemory | NodeKind::InitialVar(_) => "\"#1a3a5c\"",

        NodeKind::ControlState => "\"#2a1a4a\"",

        NodeKind::ControlPhi(_) | NodeKind::MemPhi => "\"#163030\"",

        NodeKind::If | NodeKind::IfCase(_) => "\"#3a2a10\"",

        NodeKind::Load(_) | NodeKind::Store(_) => "\"#102030\"",

        NodeKind::StackStore { .. } | NodeKind::StackStorePhi { .. } => "\"#20182a\"", // stack-slot purple

        NodeKind::Call => "\"#3a1010\"",
        NodeKind::CallOther { .. } => "\"#3a2810\"", // amber — opaque intrinsic
        NodeKind::SegmentOp { .. } => "\"#10283a\"", // teal — address computation
        NodeKind::CPoolRef => "\"#2a1a3a\"",         // violet — JVM metadata
        NodeKind::New => "\"#103a2a\"",              // dark green — allocation

        NodeKind::PostCallMemState | NodeKind::PostCallVarState(_) => "\"#28102a\"",

        NodeKind::Return => "\"#103a10\"",

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
        NodeOutputKind::Control => return ("ctrl", "\"#00cccc\""), // aqua
        NodeOutputKind::Memory => return ("mem", "\"#cc88aa\""),   // pink
        NodeOutputKind::ControlPhi => return ("phi", "\"#dddddd\""), // white
        NodeOutputKind::OutputType(_) => {}                        // fall through
    }

    // Value edges: colour/label depend on how the consumer uses this slot.
    match dumper.graph.node_kind(consumer) {
        NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::BoolBinaryOp(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatCmpOp(_) => match input_idx {
            0 => ("lhs", "\"#4488ff\""), // blue
            1 => ("rhs", "\"#ff4444\""), // red
            _ => ("", "\"#cccccc\""),
        },

        NodeKind::IntUnaryOp(_)
        | NodeKind::BoolUnaryOp(_)
        | NodeKind::Extend(_)
        | NodeKind::CastToBool
        | NodeKind::CastToInt
        | NodeKind::Truncate
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::FloatUnaryOp(_)
        | NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt
        | NodeKind::CastToFloat => ("val", "\"#88cc88\""), // green

        NodeKind::Load(_) => match input_idx {
            0 => ("mem", "\"#cc88aa\""),
            1 => ("addr", "\"#cc88ff\""), // purple
            _ => ("", "\"#cccccc\""),
        },

        NodeKind::Store(_) => match input_idx {
            0 => ("mem", "\"#cc88aa\""),
            1 => ("addr", "\"#cc88ff\""), // purple
            2 => ("data", "\"#ff8800\""), // orange
            _ => ("", "\"#cccccc\""),
        },

        // StackStore inputs = [memory, base, data].
        NodeKind::StackStore { .. } => match input_idx {
            0 => ("mem", "\"#cc88aa\""),
            1 => ("sp", "\"#cc88ff\""),   // purple — SP base
            2 => ("data", "\"#ff8800\""), // orange
            _ => ("", "\"#cccccc\""),
        },

        // StackStorePhi inputs = [phi_token, memory, data].
        NodeKind::StackStorePhi { .. } => match input_idx {
            0 => ("phi", "\"#dddddd\""),
            1 => ("mem", "\"#cc88aa\""),
            2 => ("data", "\"#ff8800\""), // orange
            _ => ("", "\"#cccccc\""),
        },

        NodeKind::Call => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("mem", "\"#cc88aa\""),
            2 => ("target", "\"#ffdd44\""), // yellow
            _ => ("arg", "\"#ff8800\""),    // orange
        },

        // CallOther inputs = [ctrl, memory, arg0, arg1, …]
        NodeKind::CallOther { .. } => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("mem", "\"#cc88aa\""),
            _ => ("arg", "\"#ff8800\""),
        },

        // SegmentOp inputs = [segment, offset]
        NodeKind::SegmentOp { .. } => match input_idx {
            0 => ("seg", "\"#ffdd44\""), // yellow — segment selector
            1 => ("off", "\"#cc88ff\""), // purple — offset
            _ => ("", "\"#cccccc\""),
        },

        // CPoolRef / New inputs are opaque references; label them by index.
        NodeKind::CPoolRef | NodeKind::New => ("ref", "\"#ff8800\""),

        NodeKind::If => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("cond", "\"#ff44ff\""), // magenta
            _ => ("", "\"#cccccc\""),
        },

        NodeKind::Return => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("mem", "\"#cc88aa\""),
            2 => ("val", "\"#88cc88\""),
            _ => ("", "\"#cccccc\""),
        },

        NodeKind::ControlState => ("ctrl", "\"#00cccc\""),

        // ControlPhi value inputs (inputs[1+]) have NodeOutputKind::OutputType
        // and fall through here.  MemPhi inputs are all either ControlPhi-dispatch
        // (kind=ControlPhi, handled above) or Memory (kind=Memory, handled above),
        // so MemPhi never reaches this arm.
        NodeKind::ControlPhi(_) => ("in", "\"#dddddd\""),

        NodeKind::PostCallMemState | NodeKind::PostCallVarState(_) => match input_idx {
            0 => ("ctrl", "\"#00cccc\""),
            1 => ("mem", "\"#cc88aa\""),
            _ => ("", "\"#cccccc\""),
        },

        _ => ("", "\"#cccccc\""),
    }
}

// ── dumper ────────────────────────────────────────────────────────────────────

pub struct GraphDotDumper<'a, R: MemReader> {
    pub(crate) entry: NodeId,
    pub(crate) graph: &'a Graph,
    pub(crate) sleigh: &'a rsleigh::Sleigh<R>,
    pub(crate) call_clobbered: &'a [rsleigh::Vn],
}

impl<'a, R: MemReader> GraphDotDumper<'a, R> {
    fn vn_to_name(&self, vn: &rsleigh::Vn) -> io::Result<String> {
        let offset = vn.addr.off;
        let size = vn.size;
        match vn.addr.space {
            rsleigh::VnSpace::CONST => Ok(format!("{offset:#x}:{size}")),
            rsleigh::VnSpace::REGISTER => {
                let regs = self
                    .sleigh
                    .regs()
                    .map_err(|e| io::Error::other(e.to_string()))?;
                let name = regs
                    .vn_to_name(*vn)
                    .ok_or_else(|| io::Error::other(format!("register not found: {vn:?}")))?;
                Ok(name.to_string())
            }
            rsleigh::VnSpace::RAM => Ok(format!("ram[{offset:#x}]:{size}")),
            rsleigh::VnSpace::UNIQUE => Ok(format!("unique[{offset:#x}]:{size}")),
            s if s == self.sleigh.default_code_space() => Ok(format!("ram[{offset:#x}]:{size}")),
            s => Err(io::Error::other(format!("unsupported VnSpace: {s:?}"))),
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
            rsleigh::VnSpace::CONST => "const",
            rsleigh::VnSpace::REGISTER => "register",
            rsleigh::VnSpace::UNIQUE => "unique",
            rsleigh::VnSpace::RAM => "ram",
            _ => "??",
        }
    }

    /// Returns the [`NodeOutputType`] of the first value output of `node`,
    /// or `None` if it has no value output.
    fn out_type(&self, node: NodeId) -> Option<NodeOutputType> {
        self.graph
            .node_outputs(node)
            .into_iter()
            .find_map(|o| self.graph.output_kind(o).as_value())
    }

    /// Returns the [`NodeOutputType`] of the `NodeOutputId` at input index
    /// `idx` of `node`, or `None` if it is not a value output.
    fn input_type(&self, node: NodeId, idx: usize) -> Option<NodeOutputType> {
        self.graph
            .node_inputs(node)
            .into_iter()
            .nth(idx)
            .and_then(|o| self.graph.output_kind(o).as_value())
    }

    fn pretty_label(&self, node: NodeId) -> io::Result<String> {
        let kind = self.graph.node_kind(node);

        let label = match kind {
            // ── entry / structural ────────────────────────────────────────────
            NodeKind::InitialVar(var) => format!("init\n{}", self.vn_to_name(var)?),
            NodeKind::MemPhi => "φ Mem".to_string(),
            NodeKind::ControlPhi(var) => format!("φ {}", self.vn_to_name(var)?),
            NodeKind::PostCallVarState(var) => format!("post-call\n{}", self.vn_to_name(var)?),
            NodeKind::IfCase(b) => format!("if.{}", if *b { "true" } else { "false" }),

            // ── constants ─────────────────────────────────────────────────────
            NodeKind::BoolConst(v) => format!("const {v}"),
            NodeKind::IntConst(v) => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!(":{}", t.as_str()))
                    .unwrap_or_default();
                format!("const {v:#x}{ty}")
            }
            NodeKind::FloatConst(bits) => match self.out_type(node) {
                Some(NodeOutputType::F32) => {
                    let v = f32::from_bits(*bits as u32);
                    format!("const {v}:f32")
                }
                _ => {
                    let v = f64::from_bits(*bits);
                    format!("const {v}:f64")
                }
            },

            // ── memory operations ─────────────────────────────────────────────
            NodeKind::Load(space) => {
                let space = self.pretty_vnspace(*space);
                let ty = self
                    .out_type(node)
                    .map(|t| format!(" {}", t.as_str()))
                    .unwrap_or_default();
                format!("Load{ty}\n← {space}")
            }
            NodeKind::Store(space) => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and addr are 0 and 1
                let ty = self
                    .input_type(node, 2)
                    .map(|t| format!(" {}", t.as_str()))
                    .unwrap_or_default();
                format!("Store{ty}\n→ {space}")
            }
            NodeKind::StackStore { space, offset } => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and base are 0 and 1
                let ty = self
                    .input_type(node, 2)
                    .map(|t| format!(" {}", t.as_str()))
                    .unwrap_or_default();
                format!("StackStore{ty}\n→ {space}[sp{}]", signed_offset(*offset))
            }
            NodeKind::StackStorePhi { space } => {
                let space = self.pretty_vnspace(*space);
                let ty = self
                    .input_type(node, 2)
                    .map(|t| format!(" {}", t.as_str()))
                    .unwrap_or_default();
                let offsets = self.graph.stack_phi_offsets(node);
                let offsets_str = if offsets.is_empty() {
                    "?".to_string()
                } else {
                    offsets
                        .iter()
                        .map(|o| format!("sp{}", signed_offset(*o)))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("φ StackStore{ty}\n→ {space}[{offsets_str}]")
            }

            // ── casts / width changes ─────────────────────────────────────────
            NodeKind::Truncate => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("Truncate\n{from} → {to}")
            }
            NodeKind::Extend(op) => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("{op:?}\n{from} → {to}")
            }
            NodeKind::CastToBool => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                format!("Cast → bool\nfrom {from}")
            }
            NodeKind::CastToInt => {
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("Cast → {to}\nfrom bool")
            }
            NodeKind::Popcount => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("Popcount\n{from} → {to}")
            }
            NodeKind::Lzcount => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("Lzcount\n{from} → {to}")
            }
            // ── arithmetic / logical ──────────────────────────────────────────
            NodeKind::IntBinaryOp(op) => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!(":{}", t.as_str()))
                    .unwrap_or_default();
                format!("{op:?}{ty}")
            }
            NodeKind::IntUnaryOp(op) => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("{op:?}\n{from} → {to}")
            }
            NodeKind::IntCmpOp(op) => {
                let operand = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                format!("{op:?}\n{operand} → bool")
            }
            NodeKind::BoolBinaryOp(op) => format!("{op:?}:bool"),
            NodeKind::BoolUnaryOp(op) => format!("{op:?}:bool"),

            // ── float arithmetic / logical ────────────────────────────────────
            NodeKind::FloatBinaryOp(op) => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!(":{}", t.as_str()))
                    .unwrap_or_default();
                format!("{op:?}{ty}")
            }
            NodeKind::FloatUnaryOp(op) => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("{op:?}\n{from} \u{2192} {to}")
            }
            NodeKind::FloatCmpOp(op) => {
                let operand = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                format!("{op:?}\n{operand} \u{2192} bool")
            }

            // ── float / integer conversions ───────────────────────────────────
            NodeKind::IntToFloat => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("IntToFloat\n{from} \u{2192} {to}")
            }
            NodeKind::FloatToInt => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("FloatToInt\n{from} \u{2192} {to}")
            }
            NodeKind::FloatToFloat => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("FloatToFloat\n{from} \u{2192} {to}")
            }
            NodeKind::IntBitsToFloat => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("bitcast\n{from} \u{2192} {to}")
            }
            NodeKind::FloatBitsToInt => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("bitcast\n{from} \u{2192} {to}")
            }
            NodeKind::CastToFloat => {
                let from = self.input_type(node, 0).map(|t| t.as_str()).unwrap_or("?");
                let to = self.out_type(node).map(|t| t.as_str()).unwrap_or("?");
                format!("CastToFloat\n{from} \u{2192} {to}")
            }

            // ── user-defined / opaque opcodes ────────────────────────────────
            NodeKind::CallOther { user_op_id } => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!("\n→ {}", t.as_str()))
                    .unwrap_or_default();
                format!("CallOther #{user_op_id}{ty}")
            }
            NodeKind::SegmentOp { op_id } => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!(":{}", t.as_str()))
                    .unwrap_or_default();
                format!("SegmentOp #{op_id}{ty}")
            }
            NodeKind::CPoolRef => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!(":{}", t.as_str()))
                    .unwrap_or_default();
                format!("CPoolRef{ty}")
            }
            NodeKind::New => {
                let ty = self
                    .out_type(node)
                    .map(|t| format!(":{}", t.as_str()))
                    .unwrap_or_default();
                format!("New{ty}")
            }

            // ── everything else ───────────────────────────────────────────────
            _ => format!("{kind:?}"),
        };

        Ok(label)
    }

    fn emit_const_node(&self, node: NodeId, dot_id: &str, out: &mut dot::DotEmitter) {
        let kind = self.graph.node_kind(node);
        let fc = node_fillcolor(kind);
        // Use pretty_label so const nodes get their type annotation too.
        let label = self
            .pretty_label(node)
            .unwrap_or_else(|_| format!("{kind:?}"));
        out.node(dot_id, &label, "ellipse", &[("fillcolor", fc)]);
    }

    /// Emits an `InitialVar` node at the given dot id, for inline duplication
    /// as the SP-base of `StackStore` consumers.  Keeps the visual style
    /// identical to the shared `InitialVar` rendering.
    fn emit_initial_var_node(&self, node: NodeId, dot_id: &str, out: &mut dot::DotEmitter) {
        let kind = self.graph.node_kind(node);
        let shape = node_shape(kind);
        let fc = node_fillcolor(kind);
        let label = self
            .pretty_label(node)
            .unwrap_or_else(|_| format!("{kind:?}"));
        out.node(dot_id, &label, shape, &[("fillcolor", fc)]);
    }

    /// Returns the register name for a clobbered call output using the
    /// `call_clobbered` map: the i-th clobbered output (output_index - 2)
    /// corresponds to the i-th vn in the map entry for that call node.
    fn call_clobbered_name(&self, output_id: NodeOutputId) -> io::Result<String> {
        let (_call_id, output_index) = self.graph.output_definition(output_id);
        let i = (output_index - 2) as usize;
        let vn = &self.call_clobbered[i];
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
    type Node = NodeId;
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
        let shape = node_shape(kind);
        let fc = node_fillcolor(kind);

        out.node(
            &cur_id,
            &self.pretty_label(node)?,
            shape,
            &[("fillcolor", fc)],
        );

        // ── Virtual nodes for structured outputs ──────────────────────────────

        // For the two control outputs of an If node, emit "if.true" and
        // "if.false" virtual nodes so each branch is clearly labelled.
        if matches!(kind, NodeKind::If) {
            let outputs = self.graph.node_outputs(node);
            let branch_labels = ["if.true", "if.false"];
            let edge_labels = ["true", "false"];
            for ((out_id, blabel), elabel) in outputs
                .into_iter()
                .zip(branch_labels.iter())
                .zip(edge_labels.iter())
            {
                // A consumer rendered before this If may have already created
                // the virtual node eagerly.  Reuse it to avoid a duplicate
                // declaration; only emit `node` when creating for the first time.
                let virt_id = match state.virtual_nodes.get(&out_id).cloned() {
                    Some(existing) => existing,
                    None => {
                        let v = state.alloc_virtual_id();
                        out.node(&v, blabel, "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
                        state.virtual_nodes.insert(out_id, v.clone());
                        v
                    }
                };
                out.edge(
                    &cur_id,
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

        // ── Draw edges from this node's inputs to this node ───────────────────

        for (idx, parent_output) in self.graph.node_inputs(node).into_iter().enumerate() {
            let parent_id = self.graph.get_node_from_output(parent_output);
            let parent_kind = *self.graph.node_kind(parent_id);

            // Inline the SP `InitialVar` into each StackStore/StackStorePhi
            // consumer: otherwise every stack store edges back to a single
            // shared node, which turns the graph into a visual hub.
            let inline_initial_var = matches!(parent_kind, NodeKind::InitialVar(_))
                && matches!(
                    kind,
                    NodeKind::StackStore { .. } | NodeKind::StackStorePhi { .. }
                )
                && idx == 1;

            // If the producing output has a virtual node, connect from it.
            // For clobbered Call outputs (index >= 2), create the virtual node
            // on the fly the first time a consumer is encountered.
            let parent_dot_id = if inline_initial_var {
                let v = state.alloc_virtual_id();
                self.emit_initial_var_node(parent_id, &v, out);
                v
            } else {
                let maybe_virt = state.virtual_nodes.get(&parent_output).cloned();
                if let Some(virt_id) = maybe_virt {
                    virt_id
                } else if parent_kind == NodeKind::Call {
                    let (_, output_index) = self.graph.output_definition(parent_output);
                    if output_index >= 2 {
                        let name = self.call_clobbered_name(parent_output)?;
                        let label = format!("Post Call\n{name}");
                        let virt_id = state.alloc_virtual_id();
                        let call_dot_id = state.get_dot_id(self.graph, parent_id);
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
                        state.get_dot_id(self.graph, parent_id)
                    }
                } else if *self.graph.node_kind(parent_id) == NodeKind::If {
                    // The If node may not have been rendered yet.  Create the
                    // virtual branch node eagerly so this consumer's edge lands
                    // on "if.true"/"if.false" rather than directly on the If
                    // diamond, which would leave the virtual node dangling.
                    let (_, output_index) = self.graph.output_definition(parent_output);
                    let blabel = if output_index == 0 {
                        "if.true"
                    } else {
                        "if.false"
                    };
                    match state.virtual_nodes.get(&parent_output).cloned() {
                        Some(existing) => existing,
                        None => {
                            let v = state.alloc_virtual_id();
                            out.node(&v, blabel, "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
                            state.virtual_nodes.insert(parent_output, v.clone());
                            v
                        }
                    }
                } else {
                    state.get_dot_id(self.graph, parent_id)
                }
            };

            let (label, color) = edge_style(self, node, idx, parent_output);

            // Numbered Call arg labels: inputs[0..2] are ctrl/mem/target,
            // so arg N lives at inputs[3 + N].  CallOther has no target, so
            // args start at inputs[2].  CPoolRef / New inputs are all "ref N".
            let owned_label: Option<String> = if matches!(kind, NodeKind::Call) && idx >= 3 {
                Some(format!("arg{}", idx - 3))
            } else if matches!(kind, NodeKind::CallOther { .. }) && idx >= 2 {
                Some(format!("arg{}", idx - 2))
            } else if matches!(kind, NodeKind::CPoolRef | NodeKind::New) {
                Some(format!("ref{idx}"))
            } else {
                None
            };
            let label_str: &str = owned_label.as_deref().unwrap_or(label);

            let mut extra: Vec<(&str, &str)> = vec![("color", color)];
            if !label_str.is_empty() {
                extra.push(("label", label_str));
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

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graph::Graph,
        node::{NodeKind, NodeOutputKind, NodeOutputType},
    };
    use dot::GraphDotDumper as _;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Creates a probe `Sleigh` context backed by an empty buffer.
    /// Sufficient for all dot tests (no instructions decoded).
    fn probe_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
        let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        rsleigh::Sleigh::new(
            rsleigh::sla_spec::SLA_SPEC_X86_64,
            rsleigh::pspec::PSPEC_X86_64,
            probe,
        )
        .expect("create probe Sleigh")
    }

    /// Renders every node reachable from `entry` and returns the DOT string.
    fn render(graph: &Graph, entry: NodeId) -> String {
        let sleigh = probe_sleigh();
        let dumper = GraphDotDumper {
            entry,
            graph,
            sleigh: &sleigh,
            call_clobbered: &[],
        };
        use dot::GraphDot;
        GraphDot::new(dumper, dot::DotStyle::empty())
            .as_dot()
            .expect("render must succeed")
    }

    /// Counts lines matching `pred` in `s`.
    fn count_lines<'a>(s: &'a str, pred: impl Fn(&'a str) -> bool) -> usize {
        s.lines().filter(|l| pred(l)).count()
    }

    /// Returns all DOT node-declaration lines (contain `[label=` but not `->`)
    fn node_decls(dot: &str) -> Vec<&str> {
        dot.lines()
            .filter(|l| l.contains("[label=") && !l.contains("->"))
            .collect()
    }

    /// Returns all DOT edge lines (contain `->`)
    fn edge_lines(dot: &str) -> Vec<&str> {
        dot.lines().filter(|l| l.contains("->")).collect()
    }

    // ── determinism ───────────────────────────────────────────────────────────

    /// Rendering the same graph twice must produce identical DOT output.
    #[test]
    fn dot_output_is_deterministic() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();

        let cs = graph.create_node(
            NodeKind::ControlState,
            [ctrl],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        let [cs_ctrl, _] = graph.node_outputs_exact::<2>(cs).unwrap();
        graph.create_node(NodeKind::Return, [cs_ctrl], []);

        let first = render(&graph, entry);
        let second = render(&graph, entry);
        assert_eq!(
            first, second,
            "same graph must render identically on two calls"
        );
    }

    /// A graph with a diamond (If → two branches → merge) must render
    /// deterministically regardless of walk order.
    #[test]
    fn dot_output_diamond_is_deterministic() {
        let mut graph = Graph::new();

        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let cond = graph.create_node(
            NodeKind::BoolConst(false),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        let [cond_out] = graph.node_outputs_exact::<1>(cond).unwrap();
        let if_node = graph.create_node(
            NodeKind::If,
            [entry_ctrl, cond_out],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [true_ctrl, false_ctrl] = graph.node_outputs_exact::<2>(if_node).unwrap();

        graph.create_node(NodeKind::Return, [true_ctrl], []);
        graph.create_node(NodeKind::Return, [false_ctrl], []);

        let first = render(&graph, entry);
        let second = render(&graph, entry);
        assert_eq!(first, second);
    }

    // ── structural correctness ────────────────────────────────────────────────

    /// The DOT output must begin with `digraph` and end with `}`.
    #[test]
    fn dot_output_has_digraph_wrapper() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let dot = render(&graph, entry);
        assert!(
            dot.trim_start().starts_with("digraph"),
            "must start with 'digraph':\n{dot}"
        );
        assert!(dot.trim_end().ends_with('}'), "must end with '}}':\n{dot}");
    }

    /// Every declared node id referenced on an edge must also appear as a node
    /// declaration (no edge references an id that was never declared).
    #[test]
    fn all_edge_endpoints_are_declared() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let cond = graph.create_node(
            NodeKind::BoolConst(true),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        let [cond_out] = graph.node_outputs_exact::<1>(cond).unwrap();
        let if_node = graph.create_node(
            NodeKind::If,
            [ctrl, cond_out],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [tc, fc] = graph.node_outputs_exact::<2>(if_node).unwrap();
        graph.create_node(NodeKind::Return, [tc], []);
        graph.create_node(NodeKind::Return, [fc], []);

        let dot = render(&graph, entry);

        // Collect every declared dot node id (the part before the first space on
        // a `"id" [label=…]` line).
        let declared: std::collections::HashSet<&str> = dot
            .lines()
            .filter(|l| l.contains("[label=") && !l.contains("->"))
            .filter_map(|l| l.trim().split('"').nth(1))
            .collect();

        // For every edge `"a" -> "b"` check both endpoints are declared.
        for line in dot.lines().filter(|l| l.contains("->")) {
            let parts: Vec<&str> = line.trim().split("->").collect();
            if parts.len() < 2 {
                continue;
            }
            let src = parts[0].trim().trim_matches('"');
            // rhs may have attributes after the id like `"b" [color=…]`
            let dst = parts[1].trim().split('"').nth(1).unwrap_or("").trim();
            assert!(
                declared.contains(src),
                "edge source '{src}' has no node declaration:\n{dot}"
            );
            assert!(
                declared.contains(dst),
                "edge destination '{dst}' has no node declaration:\n{dot}"
            );
        }
    }

    /// A linear chain (Entry → ControlState → Return) must produce exactly
    /// those three node declarations and two edges.
    #[test]
    fn linear_chain_node_and_edge_count() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let cs = graph.create_node(
            NodeKind::ControlState,
            [ctrl],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        let [cs_ctrl, _] = graph.node_outputs_exact::<2>(cs).unwrap();
        graph.create_node(NodeKind::Return, [cs_ctrl], []);

        let dot = render(&graph, entry);
        assert_eq!(
            node_decls(&dot).len(),
            3,
            "exactly 3 node declarations:\n{dot}"
        );
        assert_eq!(edge_lines(&dot).len(), 2, "exactly 2 edges:\n{dot}");
    }

    /// An `If` node must produce exactly two virtual-node declarations
    /// ("if.true" and "if.false") and exactly two edges from the `If` diamond.
    #[test]
    fn if_node_produces_exactly_two_branch_virtual_nodes() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let cond = graph.create_node(
            NodeKind::BoolConst(true),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        let [cond_out] = graph.node_outputs_exact::<1>(cond).unwrap();
        let if_node = graph.create_node(
            NodeKind::If,
            [ctrl, cond_out],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [tc, fc] = graph.node_outputs_exact::<2>(if_node).unwrap();
        graph.create_node(NodeKind::Return, [tc], []);
        graph.create_node(NodeKind::Return, [fc], []);

        let dot = render(&graph, entry);

        let if_true_count = count_lines(&dot, |l| l.contains("if.true") && l.contains("[label="));
        let if_false_count = count_lines(&dot, |l| l.contains("if.false") && l.contains("[label="));
        assert_eq!(if_true_count, 1, "exactly one if.true declaration:\n{dot}");
        assert_eq!(
            if_false_count, 1,
            "exactly one if.false declaration:\n{dot}"
        );
    }

    // ── label content ─────────────────────────────────────────────────────────

    /// `MemPhi` nodes must render with the label "φ Mem".
    #[test]
    fn mem_phi_label_is_phi_mem() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem_phi = graph.create_node(NodeKind::MemPhi, [], [NodeOutputKind::Memory]);
        // mem_phi is only reachable as a data input of Return (graph walk follows inputs)
        let [mp_out] = graph.node_outputs_exact::<1>(mem_phi).unwrap();
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        graph.create_node(NodeKind::Return, [entry_ctrl, mp_out], []);

        let dot = render(&graph, entry);
        assert!(
            dot.contains("φ Mem"),
            "MemPhi label must be 'φ Mem':\n{dot}"
        );
        assert!(
            !dot.contains("MemPhi"),
            "old 'MemPhi' label must not appear:\n{dot}"
        );
    }

    /// `IntConst` nodes must include their hex value and type in the label.
    #[test]
    fn int_const_label_contains_value_and_type() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let c = graph.create_node(
            NodeKind::IntConst(0xdeadbeef),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let [c_out] = graph.node_outputs_exact::<1>(c).unwrap();
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        graph.create_node(NodeKind::Return, [entry_ctrl, c_out], []);

        let dot = render(&graph, entry);
        assert!(
            dot.contains("0xdeadbeef"),
            "hex value must be in label:\n{dot}"
        );
        assert!(dot.contains("u32"), "type must be in label:\n{dot}");
    }

    // ── if virtual-node ordering regression ───────────────────────────────────

    /// Verify that "if.true"/"if.false" virtual nodes are correctly wired even
    /// when a branch successor (CS_true / CS_false) is rendered *before* the
    /// `If` node itself.
    ///
    /// This is the ordering scenario that used to produce a dangling "if.true"
    /// trapezium with no outgoing edge and a spurious direct edge from the `If`
    /// diamond to the true-branch ControlState (3 children on the `If` node).
    #[test]
    fn if_virtual_nodes_connected_when_consumer_rendered_before_if() {
        let mut graph = Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
        let cond_node = graph.create_node(
            NodeKind::BoolConst(true),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::Bool)],
        );
        let [cond] = graph.node_outputs_exact::<1>(cond_node).unwrap();
        let if_node = graph.create_node(
            NodeKind::If,
            [entry_ctrl, cond],
            [NodeOutputKind::Control, NodeOutputKind::Control],
        );
        let [true_ctrl, false_ctrl] = graph.node_outputs_exact::<2>(if_node).unwrap();

        let cs_true = graph.create_node(
            NodeKind::ControlState,
            [],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        graph.add_node_input(cs_true, true_ctrl).unwrap();
        let [cs_true_ctrl, _] = graph.node_outputs_exact::<2>(cs_true).unwrap();

        let cs_false = graph.create_node(
            NodeKind::ControlState,
            [],
            [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
        );
        graph.add_node_input(cs_false, false_ctrl).unwrap();
        let [cs_false_ctrl, _] = graph.node_outputs_exact::<2>(cs_false).unwrap();

        graph.create_node(NodeKind::Return, [cs_true_ctrl], []);
        graph.create_node(NodeKind::Return, [cs_false_ctrl], []);

        let sleigh = probe_sleigh();
        let dumper = GraphDotDumper {
            entry,
            graph: &graph,
            sleigh: &sleigh,
            call_clobbered: &[],
        };

        let style = dot::DotStyle::empty();
        let mut emitter = dot::DotEmitter::new("test", &style);
        let mut state = dumper.create_initial_state();

        // Render cs_true *before* if_node to trigger the historical bug.
        dumper
            .dump_as_dot(cs_true, &mut emitter, &mut state)
            .unwrap();
        dumper
            .dump_as_dot(if_node, &mut emitter, &mut state)
            .unwrap();

        let dot = emitter.finish();

        let if_true_id = dot
            .lines()
            .find_map(|line| {
                if line.contains("if.true") && line.contains("[label=") {
                    line.trim().split('"').nth(1).map(str::to_owned)
                } else {
                    None
                }
            })
            .expect("if.true node must be declared in the DOT output");

        let q = format!("\"{}\"", if_true_id);
        let edges_into = edge_lines(&dot)
            .into_iter()
            .filter(|l| l.split("->").nth(1).is_some_and(|rhs| rhs.contains(&q)))
            .count();
        let edges_from = edge_lines(&dot)
            .into_iter()
            .filter(|l| l.split("->").next().is_some_and(|lhs| lhs.contains(&q)))
            .count();

        assert!(
            edges_into >= 1,
            "if.true must have ≥1 incoming edge:\n{dot}"
        );
        assert!(
            edges_from >= 1,
            "if.true must have ≥1 outgoing edge:\n{dot}"
        );
    }
}
