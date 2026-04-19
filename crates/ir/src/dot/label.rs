use rsleigh::MemReader;
use std::io;

use super::{GraphDotDumper, node_fillcolor, node_shape};
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};

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

    pub(super) fn pretty_label(&self, node: NodeId) -> io::Result<String> {
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

    pub(super) fn emit_const_node(&self, node: NodeId, dot_id: &str, out: &mut dot::DotEmitter) {
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
    pub(super) fn emit_initial_var_node(
        &self,
        node: NodeId,
        dot_id: &str,
        out: &mut dot::DotEmitter,
    ) {
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
    pub(super) fn call_clobbered_name(&self, output_id: NodeOutputId) -> io::Result<String> {
        let (_call_id, output_index) = self.graph.output_definition(output_id);
        let i = (output_index - 2) as usize;
        let vn = &self.call_clobbered[i];
        self.vn_to_name(vn)
    }
}
