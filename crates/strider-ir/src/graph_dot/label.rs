use rsleigh::MemReader;
use std::io;

use super::{GraphDotDumper, node_fillcolor, node_shape};
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};

/// Render a varnode to its display name by delegating to rsleigh's
/// [`rsleigh::Vn::ctx_fmt`].  REGISTER
/// varnodes whose byte range matches a named register resolve to the
/// register name (e.g. `"RAX"`); every other varnode renders as
/// `<space-name>[0x<off>]:<size>` for non-CONST spaces, or `0x<off>:<size>`
/// for CONST.  Unknown space-shortcut bytes fall back to the raw
/// shortcut character via [`rsleigh::VnSpace`]'s `Display`.
///
/// # Errors
///
/// Propagates `sleigh.regs()` failures.  The format itself is infallible:
/// rsleigh's `VnCtxFmt` covers every space variant via `space_info` /
/// shortcut-character fallback, so the previous `InvalidRegVn` and
/// `UnsupportedVnSpaceDisplay` error paths no longer fire — those inputs
/// now produce a best-effort fallback string.
pub fn vn_to_display_name<R: MemReader>(
    sleigh: &rsleigh::Sleigh<R>,
    vn: &rsleigh::Vn,
) -> anyhow::Result<String> {
    let regs = sleigh.regs()?;
    Ok(vn.ctx_fmt(sleigh, &regs).to_string())
}

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
        vn_to_display_name(self.sleigh, vn).map_err(|e| io::Error::other(e.to_string()))
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
            .iter()
            .find_map(|&o| self.graph.output_kind(o).as_value())
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

    /// Type name of the first value output of `node`, or `"?"` if absent.
    fn out_type_str(&self, node: NodeId) -> &'static str {
        self.out_type(node).map_or("?", NodeOutputType::as_str)
    }

    /// Type name of input `idx` of `node`, or `"?"` if absent / non-value.
    fn input_type_str(&self, node: NodeId, idx: usize) -> &'static str {
        self.input_type(node, idx)
            .map_or("?", NodeOutputType::as_str)
    }

    /// Type suffix `"<sep><name>"` for the first value output of `node`, or an
    /// empty string when the node has no value output. `sep` is typically
    /// `":"`, `" "`, or `"\n→ "` depending on the rendering convention.
    fn out_type_suffix(&self, node: NodeId, sep: &str) -> String {
        self.out_type(node)
            .map_or_else(String::new, |t| format!("{sep}{}", t.as_str()))
    }

    /// Same as [`Self::out_type_suffix`] but reads input slot `idx` instead.
    fn input_type_suffix(&self, node: NodeId, idx: usize, sep: &str) -> String {
        self.input_type(node, idx)
            .map_or_else(String::new, |t| format!("{sep}{}", t.as_str()))
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
            NodeKind::Phi => match self.graph.phi_var_tag(node) {
                None => "φ Val".to_string(),
                Some(var) => format!("φ {}", self.vn_to_name(&var)?),
            },

            // ── constants ─────────────────────────────────────────────────────
            NodeKind::BoolConst(v) => format!("const {v}"),
            NodeKind::IntConst(v) => {
                let ty = self.out_type_suffix(node, ":");
                format!("const {v:#x}{ty}")
            }
            NodeKind::FloatConst(bits) => match self.out_type(node) {
                Some(NodeOutputType::F32) => {
                    let v = f32::from_bits(*bits as u32);
                    format!("const {v}:f32")
                }
                Some(NodeOutputType::F80) => {
                    // F80 (x87 extended precision) has no native Rust type;
                    // display the raw bit pattern.  In practice F80
                    // FloatConst nodes don't get created (the bit-conversion
                    // builders skip the immediate-fold for F80), but if
                    // one ever appears we don't want a crash or a silently-
                    // misformatted f64 label.
                    format!("const {bits:#x}:f80")
                }
                _ => {
                    let v = f64::from_bits(*bits);
                    format!("const {v}:f64")
                }
            },

            // ── memory operations ─────────────────────────────────────────────
            NodeKind::Load(space) => {
                let space = self.pretty_vnspace(*space);
                let ty = self.out_type_suffix(node, " ");
                format!("Load{ty}\n← {space}")
            }
            NodeKind::Store(space) => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and addr are 0 and 1
                let ty = self.input_type_suffix(node, 2, " ");
                format!("Store{ty}\n→ {space}")
            }
            NodeKind::StackStore { space, offset } => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and base are 0 and 1
                let ty = self.input_type_suffix(node, 2, " ");
                format!("StackStore{ty}\n→ {space}[sp{}]", signed_offset(*offset))
            }
            NodeKind::StackStorePhi { space } => {
                let space = self.pretty_vnspace(*space);
                let ty = self.input_type_suffix(node, 2, " ");
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
                let from = self.input_type_str(node, 0);
                let to = self.out_type_str(node);
                format!("Truncate\n{from} → {to}")
            }
            NodeKind::Extend(op) => {
                let from = self.input_type_str(node, 0);
                let to = self.out_type_str(node);
                format!("{op:?}\n{from} → {to}")
            }
            NodeKind::CastToBool => format!("Cast → bool\nfrom {}", self.input_type_str(node, 0)),
            NodeKind::CastToInt => format!(
                "Cast → {}\nfrom {}",
                self.out_type_str(node),
                self.input_type_str(node, 0),
            ),
            NodeKind::Popcount => {
                let from = self.input_type_str(node, 0);
                let to = self.out_type_str(node);
                format!("Popcount\n{from} → {to}")
            }
            NodeKind::Lzcount => {
                let from = self.input_type_str(node, 0);
                let to = self.out_type_str(node);
                format!("Lzcount\n{from} → {to}")
            }
            // ── arithmetic / logical ──────────────────────────────────────────
            NodeKind::IntBinaryOp(op) => format!("{op:?}{}", self.out_type_suffix(node, ":")),
            NodeKind::IntUnaryOp(op) => format!(
                "{op:?}\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),
            NodeKind::IntCmpOp(op) => {
                format!("{op:?}\n{} → bool", self.input_type_str(node, 0))
            }
            NodeKind::BoolBinaryOp(op) => format!("{op:?}:bool"),
            NodeKind::BoolUnaryOp(op) => format!("{op:?}:bool"),

            // ── float arithmetic / logical ────────────────────────────────────
            NodeKind::FloatBinaryOp(op) => format!("{op:?}{}", self.out_type_suffix(node, ":")),
            NodeKind::FloatUnaryOp(op) => format!(
                "{op:?}\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),
            NodeKind::FloatCmpOp(op) => {
                format!("{op:?}\n{} → bool", self.input_type_str(node, 0))
            }

            // ── float / integer conversions ───────────────────────────────────
            NodeKind::IntToFloat => {
                let from = self.input_type_str(node, 0);
                let to = self.out_type_str(node);
                format!("IntToFloat\n{from} → {to}")
            }
            NodeKind::FloatToInt => format!(
                "FloatToInt\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),
            NodeKind::FloatToFloat => format!(
                "FloatToFloat\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),
            NodeKind::IntBitsToFloat | NodeKind::FloatBitsToInt => format!(
                "bitcast\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),
            NodeKind::CastToFloat => format!(
                "CastToFloat\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),

            // ── user-defined / opaque opcodes ────────────────────────────────
            NodeKind::CallOther { user_op_id } => {
                // Show the resolved Sleigh user-op name when the analyzer
                // recorded one (e.g. `setISAMode #62`); fall back to the bare
                // id for synthetic nodes (tests, third-party graph builders)
                // that bypass the name side-table.
                let name_prefix = self
                    .graph
                    .call_other_name(node)
                    .map(|n| format!("{n} "))
                    .unwrap_or_default();
                format!(
                    "CallOther {name_prefix}#{user_op_id}{}",
                    self.out_type_suffix(node, "\n→ "),
                )
            }
            NodeKind::SegmentOp { op_id } => {
                format!("SegmentOp #{op_id}{}", self.out_type_suffix(node, ":"))
            }
            NodeKind::CPoolRef => format!("CPoolRef{}", self.out_type_suffix(node, ":")),
            NodeKind::New => format!("New{}", self.out_type_suffix(node, ":")),

            // ── everything else ───────────────────────────────────────────────
            _ => format!("{kind:?}"),
        };

        Ok(label)
    }

    pub(super) fn emit_const_node(&self, node: NodeId, dot_id: &str, out: &mut ::dot::DotEmitter) {
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
        out: &mut ::dot::DotEmitter,
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
    ///
    /// Falls back to a synthetic `clobN` / `outN` label when the caller's
    /// `call_clobbered` slice is shorter than the Call's actual clobbered
    /// output count, or when called with an output_index < 2 (the
    /// Control/Memory outputs). Both fallback paths exist so dot rendering
    /// of synthetic test graphs (which often pass `call_clobbered: &[]`)
    /// does not panic.
    pub(super) fn call_clobbered_name(&self, output_id: NodeOutputId) -> io::Result<String> {
        let (_call_id, output_index) = self.graph.output_definition(output_id);
        let Some(i) = output_index.checked_sub(2).map(|i| i as usize) else {
            return Ok(format!("out{output_index}"));
        };
        match self.call_clobbered.get(i) {
            Some(vn) => self.vn_to_name(vn),
            None => Ok(format!("clob{i}")),
        }
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
