use super::{FunctionDotDumper, node_fillcolor};
use crate::IRViewer;
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

/// A REGISTER varnode matching a named register renders as that name (`"RAX"`);
/// anything else as `<space>[0x<off>]:<size>`, or `0x<off>:<size>` for CONST.
fn vn_to_display_name(names: &strider_target::ArchNames, vn: &rsleigh::Vn) -> String {
    names.vn(*vn).to_string()
}

impl FunctionDotDumper<'_> {
    fn vn_to_name(&self, vn: &rsleigh::Vn) -> String {
        vn_to_display_name(self.names, vn)
    }

    fn out_type(&self, node: NodeId) -> Option<ValueType> {
        self.function
            .first_value_output_of(node)
            .and_then(|o| self.function.value_type_opt(o))
    }

    fn input_type(&self, node: NodeId, idx: usize) -> Option<ValueType> {
        self.function
            .node_inputs(node)
            .into_iter()
            .nth(idx)
            .and_then(|o| self.function.value_type_opt(o))
    }

    fn out_type_str(&self, node: NodeId) -> String {
        self.out_type(node)
            .map_or_else(|| "?".to_owned(), |t| t.to_string())
    }

    fn input_type_str(&self, node: NodeId, idx: usize) -> String {
        self.input_type(node, idx)
            .map_or_else(|| "?".to_owned(), |t| t.to_string())
    }

    /// `"<sep><type>"`, or empty when the node has no value output. `sep` is
    /// typically `":"`, `" "`, or `"\n-> "`.
    fn out_type_suffix(&self, node: NodeId, sep: &str) -> String {
        self.out_type(node)
            .map_or_else(String::new, |t| format!("{sep}{t}"))
    }

    /// [`Self::out_type_suffix`] for input slot `idx`.
    fn input_type_suffix(&self, node: NodeId, idx: usize, sep: &str) -> String {
        self.input_type(node, idx)
            .map_or_else(String::new, |t| format!("{sep}{t}"))
    }

    /// Appends a `base sp +/- K` line to a Store/Load label when the node has a
    /// Stack-class `memory_offsets` entry.  The base may be the entry SP
    /// or a masked SP.
    fn with_sp_offset(&self, node: NodeId, label: String) -> String {
        match self.function.stack_offset(node) {
            Some((_, k)) if k < 0 => format!("{label}\nbase sp - {}", -k),
            Some((_, k)) => format!("{label}\nbase sp + {k}"),
            None => label,
        }
    }

    pub(super) fn pretty_label(&self, node: NodeId) -> String {
        let kind = self.function.node_kind(node);

        match kind {
            NodeKind::InitialVar(id) => match self.function.initial_vn_opt(*id) {
                Some(vn) => format!("init\n{}", self.vn_to_name(&vn)),
                None => format!("init\n#{}", id.index()),
            },
            NodeKind::MemPhi => "φ Mem".to_string(),
            // An output-less Phi (malformed graph) labels rather than panics.
            NodeKind::Phi => match self
                .function
                .first_value_output_of(node)
                .and_then(|v| self.function.get_vn_for_value(v))
            {
                None => "φ Val".to_string(),
                Some(var) => format!("φ {}", self.vn_to_name(&var)),
            },

            NodeKind::IntConst(id) => {
                let out_ty = self.out_type(node);
                // A dangling id (malformed graph) labels rather than panics.
                match self.function.side_tables().const_interner.get(*id) {
                    None => format!("const <dangling const {id:?}>"),
                    Some(cv) if out_ty.is_some_and(|t| t.is_wide_int()) => {
                        // Stored little-endian; render high->low. The leading
                        // zeros go, so the width-padded form buys nothing.
                        let bits = out_ty.map_or(0, ValueType::bit_width);
                        let bytes = cv.to_le_bytes();
                        let raw: String = bytes.iter().rev().map(|b| format!("{b:02x}")).collect();
                        let hex = raw.trim_start_matches('0');
                        let hex = if hex.is_empty() { "0" } else { hex };
                        format!("const 0x{hex}:i{bits}")
                    }
                    Some(_) => {
                        let ty = self.out_type_suffix(node, ":");
                        // Read through the accessor, not the interned payload.
                        let v = self
                            .function
                            .first_value_output_of(node)
                            .and_then(|o| self.function.int_const_u128(o))
                            .unwrap_or(0);
                        format!("const {v:#x}{ty}")
                    }
                }
            }
            NodeKind::FloatConst(bits) => match self.out_type(node) {
                Some(ValueType::Float(32)) => {
                    let v = f32::from_bits(*bits as u32);
                    format!("const {v}:f32")
                }
                Some(ValueType::Float(64)) => {
                    let v = f64::from_bits(*bits);
                    format!("const {v}:f64")
                }
                // No native Rust carrier at these widths, so show raw bits.
                Some(ty) => format!("const {bits:#x}:{ty}"),
                None => format!("const {bits:#x}"),
            },

            NodeKind::Load(space) => {
                let space = self.names.space(*space);
                let ty = self.out_type_suffix(node, " ");
                self.with_sp_offset(node, format!("Load{ty}\n← {space}"))
            }
            NodeKind::Store(space) => {
                let space = self.names.space(*space);
                let ty = self.input_type_suffix(node, NodeKind::STORE_DATA_SLOT, " ");
                self.with_sp_offset(node, format!("Store{ty}\n-> {space}"))
            }
            NodeKind::Truncate => self.width_change_label(node, "Truncate"),
            NodeKind::Extend(op) => self.width_change_label(node, &format!("{op:?}")),
            NodeKind::Popcount => self.width_change_label(node, "Popcount"),
            NodeKind::Lzcount => self.width_change_label(node, "Lzcount"),
            NodeKind::IntBinaryOp(op) => format!("{op:?}{}", self.out_type_suffix(node, ":")),
            NodeKind::IntUnaryOp(op) => self.width_change_label(node, &format!("{op:?}")),
            NodeKind::IntCmpOp(op) => self.cmp_label(node, op),

            NodeKind::FloatBinaryOp(op) => format!("{op:?}{}", self.out_type_suffix(node, ":")),
            NodeKind::FloatUnaryOp(op) => self.width_change_label(node, &format!("{op:?}")),
            NodeKind::FloatCmpOp(op) => self.cmp_label(node, op),

            NodeKind::IntToFloat => self.width_change_label(node, "IntToFloat"),
            NodeKind::FloatToInt => self.width_change_label(node, "FloatToInt"),
            NodeKind::FloatToFloat => self.width_change_label(node, "FloatToFloat"),
            NodeKind::IntBitsToFloat | NodeKind::FloatBitsToInt => {
                self.width_change_label(node, "bitcast")
            }

            NodeKind::CallOther { user_op_id } => {
                let name_prefix = self
                    .function
                    .call_other_name(node)
                    .map(|n| format!("{n} "))
                    .unwrap_or_default();
                format!(
                    "CallOther {name_prefix}#{user_op_id}{}",
                    self.out_type_suffix(node, "\n-> "),
                )
            }
            NodeKind::CPoolRef => format!("CPoolRef{}", self.out_type_suffix(node, ":")),
            NodeKind::New => format!("New{}", self.out_type_suffix(node, ":")),

            NodeKind::Switch(_) => {
                let cases: String = self
                    .function
                    .switch_targets(node)
                    .iter()
                    .map(|a| format!("\n0x{a:x}"))
                    .collect();
                format!("Switch{cases}")
            }

            NodeKind::Call { .. } => "Call".to_string(),
            _ => format!("{kind:?}"),
        }
    }

    /// `"{prefix}\n{from} -> {to}"`, from-type off input 0 and to-type off the
    /// node's own value output.
    fn width_change_label(&self, node: NodeId, prefix: &str) -> String {
        format!(
            "{prefix}\n{} -> {}",
            self.input_type_str(node, 0),
            self.out_type_str(node),
        )
    }

    /// Comparison output is always `i1`, so only the input type varies.
    fn cmp_label(&self, node: NodeId, op: impl core::fmt::Debug) -> String {
        format!("{op:?}\n{} -> i1", self.input_type_str(node, 0))
    }

    pub(super) fn emit_const_node(&self, node: NodeId, dot_id: &str, out: &mut ::dot::DotEmitter) {
        let fc = node_fillcolor(self.function.node_kind(node));
        let label = self.pretty_label(node);
        out.node(dot_id, &label, "ellipse", &[("fillcolor", fc)]);
    }

    /// Label for a Call / CallOther output past `[Control, Memory]`, taken from
    /// the output's `value_vn` tag.  Falls back to `outN` for the two
    /// structural slots and for untagged outputs.
    pub(super) fn call_clobbered_name(&self, value_id: ValueId) -> String {
        let (_call_id, output_index) = self.function.value_definition(value_id);
        if output_index < 2 {
            return format!("out{output_index}");
        }
        match self.function.get_vn_for_value(value_id) {
            Some(vn) => self.vn_to_name(&vn),
            None => format!("out{output_index}"),
        }
    }

    /// Return inputs are `[ctrl, mem, ret_val_regs[0], ...]`, so slot `i + 2` is
    /// `ret_val_regs[i]`.  `None` when the slot is out of range of the stored
    /// convention.
    pub(super) fn return_ret_name(&self, input_slot: usize) -> Option<String> {
        let i = input_slot.checked_sub(2)?;
        let vn = *self.function.ret_val_regs().get(i)?;
        Some(self.vn_to_name(&vn))
    }
}

#[cfg(test)]
mod tests {
    use super::vn_to_display_name;
    use rsleigh::{Vn, VnSpace};

    fn names() -> &'static strider_target::ArchNames {
        strider_target::SleighArch::x86_64().names()
    }

    #[test]
    fn const_formats_as_hex_offset_colon_size() {
        let vn = Vn {
            addr_off: 0x2a,
            addr_space: VnSpace::CONST,
            size: 4,
        };
        assert_eq!(vn_to_display_name(names(), &vn), "0x2a:4");
    }

    #[test]
    fn ram_formats_as_ram_offset_size() {
        let vn = Vn {
            addr_off: 0x1000,
            addr_space: VnSpace::RAM,
            size: 8,
        };
        assert_eq!(vn_to_display_name(names(), &vn), "ram[0x1000]:8");
    }

    #[test]
    fn unique_formats_as_unique_offset_size() {
        let vn = Vn {
            addr_off: 0x80,
            addr_space: VnSpace::UNIQUE,
            size: 1,
        };
        assert_eq!(vn_to_display_name(names(), &vn), "unique[0x80]:1");
    }

    #[test]
    fn register_known_offset_returns_register_name() {
        // The register table varies across .sla versions, so try several.
        let candidates = ["RAX", "RDI", "RSI", "EAX", "AX"];
        let (name, vn) = candidates
            .iter()
            .find_map(|&n| names().regs().name_to_vn(n).map(|v| (n, v)))
            .expect("no known register resolved");
        assert_eq!(vn_to_display_name(names(), &vn), name);
    }

    #[test]
    fn register_unknown_offset_falls_back_to_space_addr_size() {
        // A REGISTER-space offset the register table won't map.
        let bogus = Vn {
            addr_off: 0xffff_ffff_ffff_ffff,
            addr_space: VnSpace::REGISTER,
            size: 1,
        };
        assert_eq!(
            vn_to_display_name(names(), &bogus),
            "register[0xffffffffffffffff]:1"
        );
    }

    #[test]
    fn unknown_space_byte_falls_back_to_shortcut_char() {
        // A shortcut that is none of CONST/REGISTER/RAM/UNIQUE.
        let exotic = Vn {
            addr_off: 0,
            addr_space: VnSpace::new(b'?'),
            size: 1,
        };
        assert_eq!(vn_to_display_name(names(), &exotic), "?[0x0]:1");
    }
}
