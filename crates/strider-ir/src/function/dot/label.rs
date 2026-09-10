use rsleigh::MemReader;

use super::{FunctionDotDumper, node_fillcolor};
use crate::IRViewer;
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

/// A REGISTER varnode matching a named register renders as that name (`"RAX"`);
/// anything else as `<space>[0x<off>]:<size>`, or `0x<off>:<size>` for CONST.
pub(crate) fn vn_to_display_name<R: MemReader>(
    sleigh: &rsleigh::Sleigh<R>,
    regs: &rsleigh::SleighRegs,
    vn: &rsleigh::Vn,
) -> String {
    vn.ctx_fmt(sleigh, regs).to_string()
}

impl<'a, R: MemReader> FunctionDotDumper<'a, R> {
    fn vn_to_name(&self, vn: &rsleigh::Vn) -> String {
        vn_to_display_name(self.sleigh, &self.regs, vn)
    }

    /// Checks the default code space first so arches whose code space isn't
    /// literally `RAM` still render as "ram".
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

    fn out_type_str(&self, node: NodeId) -> &'static str {
        self.out_type(node).map_or("?", ValueType::as_str)
    }

    fn input_type_str(&self, node: NodeId, idx: usize) -> &'static str {
        self.input_type(node, idx).map_or("?", ValueType::as_str)
    }

    /// `"<sep><type>"`, or empty when the node has no value output. `sep` is
    /// typically `":"`, `" "`, or `"\n-> "`.
    fn out_type_suffix(&self, node: NodeId, sep: &str) -> String {
        self.out_type(node)
            .map_or_else(String::new, |t| format!("{sep}{}", t.as_str()))
    }

    /// [`Self::out_type_suffix`] for input slot `idx`.
    fn input_type_suffix(&self, node: NodeId, idx: usize, sep: &str) -> String {
        self.input_type(node, idx)
            .map_or_else(String::new, |t| format!("{sep}{}", t.as_str()))
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
                match self.function.const_interner.get(*id) {
                    None => format!("const <dangling const {id:?}>"),
                    Some(_) if out_ty.is_some_and(|t| t.is_wide_int()) => {
                        // Stored little-endian; render high->low.
                        let bits = out_ty.map_or(0, ValueType::bit_width);
                        let bytes = self
                            .function
                            .int_const_wide_le_bytes(node)
                            .unwrap_or_default();
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
                Some(ValueType::F32) => {
                    let v = f32::from_bits(*bits as u32);
                    format!("const {v}:f32")
                }
                Some(ValueType::F64) => {
                    let v = f64::from_bits(*bits);
                    format!("const {v}:f64")
                }
                // No native Rust carrier at these widths, so show raw bits.
                Some(ty) => format!("const {bits:#x}:{}", ty.as_str()),
                None => format!("const {bits:#x}"),
            },

            NodeKind::Load(space) => {
                let space = self.pretty_vnspace(*space);
                let ty = self.out_type_suffix(node, " ");
                self.with_sp_offset(node, format!("Load{ty}\n← {space}"))
            }
            NodeKind::Store(space) => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and addr are 0 and 1
                let ty = self.input_type_suffix(node, 2, " ");
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
                    .side_tables()
                    .call_other_name(node)
                    .map(|n| format!("{n} "))
                    .unwrap_or_default();
                format!(
                    "CallOther {name_prefix}#{user_op_id}{}",
                    self.out_type_suffix(node, "\n-> "),
                )
            }
            NodeKind::SegmentOp { op_id } => {
                format!("SegmentOp #{op_id}{}", self.out_type_suffix(node, ":"))
            }
            NodeKind::CPoolRef => format!("CPoolRef{}", self.out_type_suffix(node, ":")),
            NodeKind::New => format!("New{}", self.out_type_suffix(node, ":")),

            NodeKind::Switch => {
                let cases: String = self
                    .function
                    .side_tables()
                    .switch_targets(node)
                    .iter()
                    .map(|a| format!("\n0x{a:x}"))
                    .collect();
                format!("Switch{cases}")
            }

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

    /// Empty-buffer probe: these tests decode no instructions.
    fn probe_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
        let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        rsleigh::Sleigh::new(
            rsleigh::sla_spec::SLA_SPEC_X86_64,
            rsleigh::pspec::PSPEC_X86_64,
            probe,
        )
        .expect("probe Sleigh")
    }

    #[test]
    fn const_formats_as_hex_offset_colon_size() {
        let sleigh = probe_sleigh();
        let vn = Vn {
            addr_off: 0x2a,
            addr_space: VnSpace::CONST,
            size: 4,
        };
        let name = vn_to_display_name(&sleigh, &sleigh.regs().unwrap(), &vn);
        assert_eq!(name, "0x2a:4");
    }

    #[test]
    fn ram_formats_as_ram_offset_size() {
        let sleigh = probe_sleigh();
        let vn = Vn {
            addr_off: 0x1000,
            addr_space: VnSpace::RAM,
            size: 8,
        };
        let name = vn_to_display_name(&sleigh, &sleigh.regs().unwrap(), &vn);
        assert_eq!(name, "ram[0x1000]:8");
    }

    #[test]
    fn unique_formats_as_unique_offset_size() {
        let sleigh = probe_sleigh();
        let vn = Vn {
            addr_off: 0x80,
            addr_space: VnSpace::UNIQUE,
            size: 1,
        };
        let name = vn_to_display_name(&sleigh, &sleigh.regs().unwrap(), &vn);
        assert_eq!(name, "unique[0x80]:1");
    }

    #[test]
    fn register_known_offset_returns_register_name() {
        let sleigh = probe_sleigh();
        let regs = sleigh.regs().expect("regs");
        // The register table varies across .sla versions, so try several.
        let candidates = ["RAX", "RDI", "RSI", "EAX", "AX"];
        let (name, vn) = candidates
            .iter()
            .find_map(|&n| regs.name_to_vn(n).map(|v| (n, v)))
            .expect("no known register resolved");
        let resolved = vn_to_display_name(&sleigh, &regs, &vn);
        assert_eq!(resolved, name);
    }

    #[test]
    fn register_unknown_offset_falls_back_to_space_addr_size() {
        let sleigh = probe_sleigh();
        // A REGISTER-space offset the register table won't map.
        let bogus = Vn {
            addr_off: 0xffff_ffff_ffff_ffff,
            addr_space: VnSpace::REGISTER,
            size: 1,
        };
        let resolved = vn_to_display_name(&sleigh, &sleigh.regs().unwrap(), &bogus);
        assert_eq!(resolved, "register[0xffffffffffffffff]:1");
    }

    #[test]
    fn unknown_space_byte_falls_back_to_shortcut_char() {
        let sleigh = probe_sleigh();
        // A shortcut that is none of CONST/REGISTER/RAM/UNIQUE.
        let exotic = Vn {
            addr_off: 0,
            addr_space: VnSpace::new(b'?'),
            size: 1,
        };
        let resolved = vn_to_display_name(&sleigh, &sleigh.regs().unwrap(), &exotic);
        assert_eq!(resolved, "?[0x0]:1");
    }
}
