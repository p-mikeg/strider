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
            NodeKind::FunctionArg { source, index } => match source {
                crate::node::FunctionArgSource::Register(reg) => {
                    format!("arg[{}] ← {}", index, self.vn_to_name(reg)?)
                }
                crate::node::FunctionArgSource::Stack { space, offset } => {
                    let space = self.pretty_vnspace(*space);
                    format!(
                        "arg[{}] ← {}[sp{}]",
                        index,
                        space,
                        signed_offset(*offset)
                    )
                }
            },
            NodeKind::MemPhi => "φ Mem".to_string(),
            NodeKind::ValuePhi => "φ Val".to_string(),
            NodeKind::ControlPhi(var) => format!("φ {}", self.vn_to_name(var)?),
            NodeKind::PostCallVarState(var) => format!("post-call\n{}", self.vn_to_name(var)?),

            // ── constants ─────────────────────────────────────────────────────
            NodeKind::BoolConst(v) => format!("const {v}"),
            NodeKind::IntConst(v) => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!(":{}", t.as_str()));
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
                    .map_or_else(String::new, |t| format!(" {}", t.as_str()));
                format!("Load{ty}\n← {space}")
            }
            NodeKind::Store(space) => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and addr are 0 and 1
                let ty = self
                    .input_type(node, 2)
                    .map_or_else(String::new, |t| format!(" {}", t.as_str()));
                format!("Store{ty}\n→ {space}")
            }
            NodeKind::StackStore { space, offset } => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and base are 0 and 1
                let ty = self
                    .input_type(node, 2)
                    .map_or_else(String::new, |t| format!(" {}", t.as_str()));
                format!("StackStore{ty}\n→ {space}[sp{}]", signed_offset(*offset))
            }
            NodeKind::StackStorePhi { space } => {
                let space = self.pretty_vnspace(*space);
                let ty = self
                    .input_type(node, 2)
                    .map_or_else(String::new, |t| format!(" {}", t.as_str()));
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
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("Truncate\n{from} → {to}")
            }
            NodeKind::Extend(op) => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("{op:?}\n{from} → {to}")
            }
            NodeKind::CastToBool => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                format!("Cast → bool\nfrom {from}")
            }
            NodeKind::CastToInt => {
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("Cast → {to}\nfrom bool")
            }
            NodeKind::Popcount => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("Popcount\n{from} → {to}")
            }
            NodeKind::Lzcount => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("Lzcount\n{from} → {to}")
            }
            // ── arithmetic / logical ──────────────────────────────────────────
            NodeKind::IntBinaryOp(op) => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!(":{}", t.as_str()));
                format!("{op:?}{ty}")
            }
            NodeKind::IntUnaryOp(op) => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("{op:?}\n{from} → {to}")
            }
            NodeKind::IntCmpOp(op) => {
                let operand = self.input_type(node, 0).map_or("?", |t| t.as_str());
                format!("{op:?}\n{operand} → bool")
            }
            NodeKind::BoolBinaryOp(op) => format!("{op:?}:bool"),
            NodeKind::BoolUnaryOp(op) => format!("{op:?}:bool"),

            // ── float arithmetic / logical ────────────────────────────────────
            NodeKind::FloatBinaryOp(op) => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!(":{}", t.as_str()));
                format!("{op:?}{ty}")
            }
            NodeKind::FloatUnaryOp(op) => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("{op:?}\n{from} \u{2192} {to}")
            }
            NodeKind::FloatCmpOp(op) => {
                let operand = self.input_type(node, 0).map_or("?", |t| t.as_str());
                format!("{op:?}\n{operand} \u{2192} bool")
            }

            // ── float / integer conversions ───────────────────────────────────
            NodeKind::IntToFloat => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("IntToFloat\n{from} \u{2192} {to}")
            }
            NodeKind::FloatToInt => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("FloatToInt\n{from} \u{2192} {to}")
            }
            NodeKind::FloatToFloat => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("FloatToFloat\n{from} \u{2192} {to}")
            }
            NodeKind::IntBitsToFloat => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("bitcast\n{from} \u{2192} {to}")
            }
            NodeKind::FloatBitsToInt => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("bitcast\n{from} \u{2192} {to}")
            }
            NodeKind::CastToFloat => {
                let from = self.input_type(node, 0).map_or("?", |t| t.as_str());
                let to = self.out_type(node).map_or("?", |t| t.as_str());
                format!("CastToFloat\n{from} \u{2192} {to}")
            }

            // ── user-defined / opaque opcodes ────────────────────────────────
            NodeKind::CallOther { user_op_id } => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!("\n→ {}", t.as_str()));
                format!("CallOther #{user_op_id}{ty}")
            }
            NodeKind::SegmentOp { op_id } => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!(":{}", t.as_str()));
                format!("SegmentOp #{op_id}{ty}")
            }
            NodeKind::CPoolRef => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!(":{}", t.as_str()));
                format!("CPoolRef{ty}")
            }
            NodeKind::New => {
                let ty = self
                    .out_type(node)
                    .map_or_else(String::new, |t| format!(":{}", t.as_str()));
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

    /// Returns the register name for a `Return` input at the given input
    /// slot.  Return inputs are `[ctrl(0), mem(1), ret_val_regs[0](2), …]`
    /// so slot `i + 2` corresponds to `ret_val_regs[i]`.  Returns `None` if
    /// the slot is out of range of the stored calling-convention ret regs
    /// (e.g. synthetic test graphs that don't carry a convention).
    pub(super) fn return_ret_name(&self, input_slot: usize) -> io::Result<Option<String>> {
        let Some(i) = input_slot.checked_sub(2) else {
            return Ok(None);
        };
        let Some(vn) = self.ret_val_regs.get(i) else {
            return Ok(None);
        };
        self.vn_to_name(vn).map(Some)
    }
}
