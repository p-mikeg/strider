use rsleigh::MemReader;
use std::io;

use super::{FunctionDotDumper, node_fillcolor};
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

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
pub(crate) fn vn_to_display_name<R: MemReader>(
    sleigh: &rsleigh::Sleigh<R>,
    vn: &rsleigh::Vn,
) -> anyhow::Result<String> {
    let regs = sleigh.regs()?;
    Ok(vn.ctx_fmt(sleigh, &regs).to_string())
}

impl<'a, R: MemReader> FunctionDotDumper<'a, R> {
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

    /// Returns the [`ValueType`] of the first value output of `node`,
    /// or `None` if it has no value output.
    fn out_type(&self, node: NodeId) -> Option<ValueType> {
        self.function
            .node_outputs(node)
            .iter()
            .find_map(|&o| self.function.value_kind(o).as_value())
    }

    /// Returns the [`ValueType`] of the `ValueId` at input index
    /// `idx` of `node`, or `None` if it is not a value output.
    fn input_type(&self, node: NodeId, idx: usize) -> Option<ValueType> {
        self.function
            .node_inputs(node)
            .into_iter()
            .nth(idx)
            .and_then(|o| self.function.value_kind(o).as_value())
    }

    /// Type name of the first value output of `node`, or `"?"` if absent.
    fn out_type_str(&self, node: NodeId) -> &'static str {
        self.out_type(node).map_or("?", ValueType::as_str)
    }

    /// Type name of input `idx` of `node`, or `"?"` if absent / non-value.
    fn input_type_str(&self, node: NodeId, idx: usize) -> &'static str {
        self.input_type(node, idx)
            .map_or("?", ValueType::as_str)
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

    /// Appends a `base sp ± K` line to a Store/Load label when the node has a
    /// `stack_offsets` entry.  The base is shown generically as `base sp`
    /// (rather than the literal `sp`) because the SP-derived base can be the
    /// entry SP *or* an alignment-masked SP — the address-input edge resolves
    /// which one concretely; this line is just the quick-read offset.
    fn with_sp_offset(&self, node: NodeId, label: String) -> String {
        match self.function.stack_offset(node) {
            Some((_, k)) if k < 0 => format!("{label}\nbase sp - {}", -k),
            Some((_, k)) => format!("{label}\nbase sp + {k}"),
            None => label,
        }
    }

    pub(super) fn pretty_label(&self, node: NodeId) -> io::Result<String> {
        let kind = self.function.node_kind(node);

        let label = match kind {
            // ── entry / structural ────────────────────────────────────────────
            NodeKind::InitialVar(var) => format!("init\n{}", self.vn_to_name(var)?),
            NodeKind::MemPhi => "φ Mem".to_string(),
            NodeKind::Phi => match self.function.phi_var_tag(node) {
                None => "φ Val".to_string(),
                Some(var) => format!("φ {}", self.vn_to_name(&var)?),
            },

            // ── constants ─────────────────────────────────────────────────────
            NodeKind::IntConst(v) => {
                let ty = self.out_type_suffix(node, ":");
                format!("const {v:#x}{ty}")
            }
            NodeKind::IntConstWide(id) => {
                // I256 / I512 payload interned in `Graph::wide_const_interner`.
                // Render the actual value (limbs are little-endian, so walk
                // high→low) rather than the Debug form of the interning id.
                // A dangling id (malformed graph) labels rather than panics.
                match self.function.graph().wide_const_opt(*id) {
                    None => format!("const <dangling wide-const {id:?}>"),
                    Some(storage) => {
                        let limbs = storage.limbs();
                        let mut hex = String::new();
                        for &limb in limbs.iter().rev() {
                            if hex.is_empty() {
                                if limb == 0 {
                                    continue;
                                }
                                hex.push_str(&format!("{limb:x}"));
                            } else {
                                hex.push_str(&format!("{limb:016x}"));
                            }
                        }
                        if hex.is_empty() {
                            hex.push('0');
                        }
                        format!("const 0x{hex}:i{}", limbs.len() * 64)
                    }
                }
            }
            NodeKind::FloatConst(bits) => match self.out_type(node) {
                Some(ValueType::F32) => {
                    let v = f32::from_bits(*bits as u32);
                    format!("const {v}:f32")
                }
                Some(ValueType::F80) => {
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
                self.with_sp_offset(node, format!("Load{ty}\n← {space}"))
            }
            NodeKind::Store(space) => {
                let space = self.pretty_vnspace(*space);
                // data is input 2; memory and addr are 0 and 1
                let ty = self.input_type_suffix(node, 2, " ");
                self.with_sp_offset(node, format!("Store{ty}\n→ {space}"))
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
                format!("{op:?}\n{} → i1", self.input_type_str(node, 0))
            }

            // ── float arithmetic / logical ────────────────────────────────────
            NodeKind::FloatBinaryOp(op) => format!("{op:?}{}", self.out_type_suffix(node, ":")),
            NodeKind::FloatUnaryOp(op) => format!(
                "{op:?}\n{} → {}",
                self.input_type_str(node, 0),
                self.out_type_str(node),
            ),
            NodeKind::FloatCmpOp(op) => {
                format!("{op:?}\n{} → i1", self.input_type_str(node, 0))
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

            // ── user-defined / opaque opcodes ────────────────────────────────
            NodeKind::CallOther { user_op_id } => {
                // Show the resolved Sleigh user-op name when the analyzer
                // recorded one (e.g. `setISAMode #62`); fall back to the bare
                // id for synthetic nodes (tests, third-party graph builders)
                // that bypass the name side-table.
                let name_prefix = self
                    .function
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
        let kind = self.function.node_kind(node);
        let fc = node_fillcolor(kind);
        // Use pretty_label so const nodes get their type annotation too.
        let label = self
            .pretty_label(node)
            .unwrap_or_else(|_| format!("{kind:?}"));
        out.node(dot_id, &label, "ellipse", &[("fillcolor", fc)]);
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
    pub(super) fn call_clobbered_name(&self, value_id: ValueId) -> io::Result<String> {
        let (_call_id, output_index) = self.function.value_definition(value_id);
        let Some(i) = output_index.checked_sub(2).map(|i| i as usize) else {
            return Ok(format!("out{output_index}"));
        };
        match self.function.call_clobbered_regs().get(i) {
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
        let Some(vn) = self.function.ret_val_regs().get(i) else {
            return Ok(None);
        };
        self.vn_to_name(vn).map(Some)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Per-VnSpace formatting tests for [`vn_to_display_name`].
    //!
    //! After the rsleigh-4 migration, `vn_to_display_name` delegates to
    //! `rsleigh::Vn::ctx_fmt`; the formerly-erroring paths
    //! (unknown-register offset, exotic-space byte) now produce a
    //! best-effort fallback string instead of an error.

    use super::vn_to_display_name;
    use rsleigh::{Vn, VnSpace};

    /// Probe `Sleigh` backed by an empty buffer.  Sufficient for the
    /// format tests — no instructions are decoded.
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
        let vn = Vn { addr_off: 0x2a, addr_space: VnSpace::CONST, size: 4 };
        let name = vn_to_display_name(&sleigh, &vn).unwrap();
        assert_eq!(name, "0x2a:4");
    }

    #[test]
    fn ram_formats_as_ram_offset_size() {
        let sleigh = probe_sleigh();
        let vn = Vn { addr_off: 0x1000, addr_space: VnSpace::RAM, size: 8 };
        let name = vn_to_display_name(&sleigh, &vn).unwrap();
        assert_eq!(name, "ram[0x1000]:8");
    }

    #[test]
    fn unique_formats_as_unique_offset_size() {
        let sleigh = probe_sleigh();
        let vn = Vn { addr_off: 0x80, addr_space: VnSpace::UNIQUE, size: 1 };
        let name = vn_to_display_name(&sleigh, &vn).unwrap();
        assert_eq!(name, "unique[0x80]:1");
    }

    #[test]
    fn register_known_offset_returns_register_name() {
        let sleigh = probe_sleigh();
        let regs = sleigh.regs().expect("regs");
        // Pick a well-known x86-64 register name; try a few until one
        // resolves (Sleigh's table varies subtly across .sla versions).
        let candidates = ["RAX", "RDI", "RSI", "EAX", "AX"];
        let (name, vn) = candidates
            .iter()
            .find_map(|&n| regs.name_to_vn(n).map(|v| (n, v)))
            .expect("no known register resolved");
        let resolved = vn_to_display_name(&sleigh, &vn).unwrap();
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
        let resolved = vn_to_display_name(&sleigh, &bogus).unwrap();
        assert_eq!(resolved, "register[0xffffffffffffffff]:1");
    }

    #[test]
    fn unknown_space_byte_falls_back_to_shortcut_char() {
        let sleigh = probe_sleigh();
        // A space shortcut that is neither CONST (#), REGISTER (%),
        // RAM (r), nor UNIQUE (u).  rsleigh renders the raw shortcut
        // char via `Display for VnSpace`.
        let exotic = Vn {
            addr_off: 0,
            addr_space: VnSpace::new(b'?'),
            size: 1,
        };
        let resolved = vn_to_display_name(&sleigh, &exotic).unwrap();
        assert_eq!(resolved, "?[0x0]:1");
    }
}
