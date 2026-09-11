//! Constant values of a machine instruction's p-code temporaries.
//!
//! A sla may address a REGISTER through the LOAD / STORE opcodes when the
//! register is chosen by an instruction field rather than named outright --
//! ARM's `vld1.N {dX[i]}, [addr]` writes lane `i` of `dX` that way. The address
//! is then a small expression over constants the decoder substituted.
//!
//! It does not always fold. ARM's multi-structure `VLD2/3/4` and `VST2/3/4`
//! build the same shape but walk it with an INTRA-INSTRUCTION loop, so the
//! pointer is loop-carried and the CFG splits the one machine instruction
//! across regions. This resets at every region boundary, which puts the
//! definition and the use on opposite sides. The lift takes an opaque path
//! there; see `memory::opaque_register_store`.
//!
//! Both the def-site collection and the lift walk `region.insns` in the same
//! order and feed every op through one of these, so the varnode they resolve
//! for a given store is the same one, and they agree on which stores resolve
//! at all. That agreement is load-bearing: a lift that writes a variable whose
//! def site was never recorded gets no phi at a join.

use rustc_hash::FxHashMap;

use strider_cfg::MachineInsnAddr;
use strider_cfg::PcodeInsnAddr;

/// Keyed by the varnode's full identity: two temporaries can share an offset
/// at different widths.
type TempKey = (u64, u32);

#[derive(Default)]
pub(crate) struct PcodeConsts {
    /// Cleared when the machine instruction changes, so a temporary cannot be
    /// read across one. Sleigh reuses unique offsets freely between them.
    at: Option<MachineInsnAddr>,
    vals: FxHashMap<TempKey, u128>,
}

impl PcodeConsts {
    /// Start of a region. `collect_def_sites` builds a fresh one per region,
    /// so the lift must clear rather than carry: a region can begin at a pcode
    /// index INSIDE a machine instruction, and a carried temporary would let
    /// the lift fold an address the def collection could not.
    pub(crate) fn reset(&mut self) {
        self.at = None;
        self.vals.clear();
    }

    /// Feed every op, in order, BEFORE handling it.
    pub(crate) fn observe(&mut self, addr: PcodeInsnAddr, insn: &rsleigh::Insn) {
        if self.at != Some(addr.machine_addr) {
            self.vals.clear();
        }
        self.at = Some(addr.machine_addr);
        let Some(out) = insn.output.as_ref() else {
            return;
        };
        if out.addr_space != rsleigh::VnSpace::UNIQUE {
            return;
        }
        // Folded BEFORE the invalidation below, which would otherwise drop an
        // input this op reads at the offset it writes (`t = t + 1`).
        let folded = self.folded(insn);
        // Drop every entry this write OVERLAPS, not just the one at the same
        // (offset, size). Sleigh reuses a unique offset at different widths,
        // and a stale wider value read back after a narrower write would fold
        // to the wrong number, which here means naming the wrong register.
        let (lo, hi) = (
            out.addr_off,
            out.addr_off.saturating_add(u64::from(out.size)),
        );
        self.vals
            .retain(|&(off, size), _| off >= hi || off.saturating_add(u64::from(size)) <= lo);
        if let Some(v) = folded {
            self.vals
                .insert((out.addr_off, out.size), mask_to(v, out.size));
        }
        // An op this does not model leaves the temporary UNKNOWN rather than
        // stale, so a later read fails closed.
    }

    /// `None` when the value is not derivable from constants alone.
    pub(crate) fn value_of(&self, vn: &rsleigh::Vn) -> Option<u128> {
        match vn.addr_space {
            rsleigh::VnSpace::CONST => Some(mask_to(u128::from(vn.addr_off), vn.size)),
            rsleigh::VnSpace::UNIQUE => self.vals.get(&(vn.addr_off, vn.size)).copied(),
            _ => None,
        }
    }

    fn folded(&self, insn: &rsleigh::Insn) -> Option<u128> {
        use rsleigh::Opcode;
        let a = || self.value_of(insn.inputs.first()?);
        let b = || self.value_of(insn.inputs.get(1)?);
        let out_size = insn.output.as_ref()?.size;
        match insn.opcode {
            Opcode::Copy => a(),
            Opcode::IntAdd => Some(a()?.wrapping_add(b()?)),
            Opcode::IntSub => Some(a()?.wrapping_sub(b()?)),
            Opcode::IntMul => Some(a()?.wrapping_mul(b()?)),
            Opcode::IntLeft => Some(shift_left(a()?, b()?, out_size)),
            Opcode::IntRight => Some(shift_right(a()?, b()?, out_size)),
            Opcode::IntOr => Some(a()? | b()?),
            Opcode::IntAnd => Some(a()? & b()?),
            Opcode::IntXor => Some(a()? ^ b()?),
            // Deliberately narrow: an unmodelled op is not an error here, only
            // an unknown, and the caller that needs a register address fails
            // closed on it.
            _ => None,
        }
    }
}

/// `opbehavior.cc:411` `OpBehaviorIntLeft`: a count at or past `8 * sizeout`
/// gives 0, not a shift by a clamped count. Past 128 the trailing [`mask_to`]
/// would not cover the difference.
fn shift_left(v: u128, count: u128, out_size: u32) -> u128 {
    shifted(count, out_size, || {
        u32::try_from(count).ok().and_then(|c| v.checked_shl(c))
    })
}

/// `opbehavior.cc:432` `OpBehaviorIntRight`, which masks the input to the
/// output width BEFORE shifting: masking only the result keeps bits that were
/// never in the output.
fn shift_right(v: u128, count: u128, out_size: u32) -> u128 {
    shifted(count, out_size, || {
        u32::try_from(count)
            .ok()
            .and_then(|c| mask_to(v, out_size).checked_shr(c))
    })
}

/// A count `8 * out_size` or more is zero; so is one past `u128`'s width,
/// where every kept bit has left the representable range.
fn shifted(count: u128, out_size: u32, f: impl FnOnce() -> Option<u128>) -> u128 {
    if count >= u128::from(out_size) * 8 {
        return 0;
    }
    f().unwrap_or(0)
}

fn mask_to(v: u128, size_bytes: u32) -> u128 {
    v & strider_ir::node::low_bits_mask_u128(size_bytes.saturating_mul(8) as usize)
}

/// The register a STORE writes, when it addresses the REGISTER space and its
/// address resolves. `None` for an ordinary memory store, and for a register
/// store this cannot name, where the lift takes its opaque path rather than
/// writing the wrong register.
pub(crate) fn register_store_target(
    insn: &rsleigh::Insn,
    consts: &PcodeConsts,
    declared: &[rsleigh::Vn],
) -> Option<rsleigh::Vn> {
    register_slot(insn, consts, declared, insn.inputs.get(2)?.size)
}

/// The register a LOAD reads, on the same terms as [`register_store_target`].
/// Width comes from the output, which is what the value is read into.
pub(crate) fn register_load_source(
    insn: &rsleigh::Insn,
    consts: &PcodeConsts,
    declared: &[rsleigh::Vn],
) -> Option<rsleigh::Vn> {
    register_slot(insn, consts, declared, insn.output.as_ref()?.size)
}

/// Whether `insn` addresses the REGISTER space at all, independent of whether
/// the address resolves. What separates "this is an ordinary memory access"
/// from "this is a register access this cannot name".
pub(crate) fn is_register_space_access(insn: &rsleigh::Insn) -> bool {
    crate::lift::pcode_util::decode_space_id(insn)
        .is_ok_and(|space| space == rsleigh::VnSpace::REGISTER)
}

/// The folded address as the exact slice it names, gated on some DECLARED
/// register enclosing it.
///
/// The gate is what keeps the tracked set a nesting family. A computed offset
/// need not land on a declared boundary, ARM's VLD4/VST4 single-lane forms
/// omitting the element-size scale so the address is a raw byte offset into
/// the register file, and an offset that straddles two registers, or lands
/// past the end of the file, names no register at all. Seeding such a slot puts a
/// varnode that only PARTIALLY overlaps a real register into the tracked set,
/// where `dedup_overlapping_largest` keeps both and models them as
/// non-aliasing: a write to one is invisible to a read of the other.
///
/// The slice itself is returned, not its container, because the width is the
/// access width. [`enclosing_register`] is what the tracked set is seeded
/// with, and `write_vn` reaches it from the slice through the container map.
fn register_slot(
    insn: &rsleigh::Insn,
    consts: &PcodeConsts,
    declared: &[rsleigh::Vn],
    size: u32,
) -> Option<rsleigh::Vn> {
    let slot = raw_register_slot(insn, consts, size)?;
    vn_container::smallest_enclosing(declared, &slot).map(|_| slot)
}

fn raw_register_slot(insn: &rsleigh::Insn, consts: &PcodeConsts, size: u32) -> Option<rsleigh::Vn> {
    if !is_register_space_access(insn) {
        return None;
    }
    let off = u64::try_from(consts.value_of(insn.inputs.get(1)?)?).ok()?;
    Some(rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: off,
        size,
    })
}

/// The declared register a resolved slot is a slice of. Seeded into the
/// tracked set so the slot and every other view of that register share one SSA
/// variable.
pub(crate) fn enclosing_register(
    declared: &[rsleigh::Vn],
    slot: &rsleigh::Vn,
) -> Option<rsleigh::Vn> {
    vn_container::smallest_enclosing(declared, slot)
}

/// The tracked varnodes an UNRESOLVABLE register-space write may reach: the
/// whole register file, the address being unknown.
pub(crate) fn opaque_clobber_set(
    all_vns: &[rsleigh::Vn],
) -> impl Iterator<Item = rsleigh::Vn> + '_ {
    all_vns
        .iter()
        .copied()
        .filter(|v| v.addr_space == rsleigh::VnSpace::REGISTER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsleigh::{Opcode, Vn, VnSpace};

    fn vn(space: VnSpace, off: u64, size: u32) -> Vn {
        Vn {
            addr_space: space,
            addr_off: off,
            size,
        }
    }

    fn konst(v: u64) -> Vn {
        vn(VnSpace::CONST, v, 4)
    }

    fn konst_of(v: u64, size: u32) -> Vn {
        vn(VnSpace::CONST, v, size)
    }

    fn op(opcode: Opcode, out: Option<Vn>, ins: &[Vn]) -> rsleigh::Insn {
        rsleigh::Insn {
            opcode,
            output: out,
            inputs: ins.iter().copied().collect(),
        }
    }

    fn at(addr: u64, index: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr },
            insn_index: index,
        }
    }

    /// The shape ARM's `vld1.32 {d0[0]}, [sp]` builds: `768 + 4 * 0`, which is
    /// `d0` at lane 0.
    #[test]
    fn a_lane_address_folds_to_its_register() {
        let t0 = vn(VnSpace::UNIQUE, 1000, 4);
        let t1 = vn(VnSpace::UNIQUE, 1004, 4);
        let t2 = vn(VnSpace::UNIQUE, 1008, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(t0), &[konst(0)]));
        c.observe(at(0x10, 1), &op(Opcode::IntMul, Some(t1), &[konst(4), t0]));
        c.observe(
            at(0x10, 2),
            &op(Opcode::IntAdd, Some(t2), &[konst(768), t1]),
        );
        assert_eq!(c.value_of(&t2), Some(768));
    }

    #[test]
    fn a_temporary_does_not_survive_into_the_next_machine_instruction() {
        let t = vn(VnSpace::UNIQUE, 1000, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(t), &[konst(7)]));
        assert_eq!(c.value_of(&t), Some(7));
        c.observe(at(0x14, 0), &op(Opcode::Nop, None, &[]));
        assert_eq!(c.value_of(&t), None, "carried across a machine instruction");
    }

    #[test]
    fn reset_clears_everything() {
        let t = vn(VnSpace::UNIQUE, 1000, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(t), &[konst(7)]));
        c.reset();
        assert_eq!(c.value_of(&t), None);
    }

    /// A narrower write over the same offset must not leave the wider value
    /// readable: folding it would name a register the instruction never wrote.
    #[test]
    fn an_overlapping_write_invalidates_the_wider_entry() {
        let wide = vn(VnSpace::UNIQUE, 1000, 8);
        let narrow = vn(VnSpace::UNIQUE, 1000, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(wide), &[konst(0x1234)]));
        assert_eq!(c.value_of(&wide), Some(0x1234));
        // An op this does not model: the narrow slot becomes unknown, and the
        // wide one it overlaps must go with it.
        c.observe(at(0x10, 1), &op(Opcode::Popcount, Some(narrow), &[wide]));
        assert_eq!(c.value_of(&wide), None);
        assert_eq!(c.value_of(&narrow), None);
    }

    #[test]
    fn a_disjoint_write_leaves_its_neighbour_alone() {
        let a = vn(VnSpace::UNIQUE, 1000, 4);
        let b = vn(VnSpace::UNIQUE, 1004, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(a), &[konst(5)]));
        c.observe(at(0x10, 1), &op(Opcode::Copy, Some(b), &[konst(6)]));
        assert_eq!(c.value_of(&a), Some(5));
        assert_eq!(c.value_of(&b), Some(6));
    }

    /// `t = t + 1` reads the offset it writes; the invalidation must not eat
    /// the input before the fold runs.
    #[test]
    fn an_op_reading_the_slot_it_writes_still_folds() {
        let t = vn(VnSpace::UNIQUE, 1000, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(t), &[konst(7)]));
        c.observe(at(0x10, 1), &op(Opcode::IntAdd, Some(t), &[t, konst(1)]));
        assert_eq!(c.value_of(&t), Some(8));
    }

    /// Sleigh returns 0 for a count at or past the output width. A count past
    /// 128 with a 16-byte output outruns the trailing mask, so a clamp names a
    /// register the instruction never wrote.
    #[test]
    fn a_shift_count_past_the_output_width_folds_to_zero() {
        let wide = vn(VnSpace::UNIQUE, 1000, 16);
        let narrow = vn(VnSpace::UNIQUE, 1100, 4);
        let mut c = PcodeConsts::default();
        c.observe(
            at(0x10, 0),
            &op(Opcode::IntLeft, Some(wide), &[konst(1), konst(200)]),
        );
        assert_eq!(c.value_of(&wide), Some(0));
        c.observe(
            at(0x10, 1),
            &op(
                Opcode::IntRight,
                Some(narrow),
                &[konst_of(0x1_0000_0000, 8), konst(32)],
            ),
        );
        assert_eq!(c.value_of(&narrow), Some(0));
    }

    /// `(in1 & calc_mask(sizeout)) >> in2`: masking after the shift instead
    /// keeps bits that were never in the output.
    #[test]
    fn a_right_shift_masks_the_input_to_the_output_width() {
        let out = vn(VnSpace::UNIQUE, 1000, 1);
        let mut c = PcodeConsts::default();
        c.observe(
            at(0x10, 0),
            &op(
                Opcode::IntRight,
                Some(out),
                &[konst_of(0xff00, 8), konst(4)],
            ),
        );
        assert_eq!(c.value_of(&out), Some(0));
    }

    #[test]
    fn a_register_is_never_a_constant() {
        let r = vn(VnSpace::REGISTER, 32, 4);
        let c = PcodeConsts::default();
        assert_eq!(c.value_of(&r), None);
    }
}
