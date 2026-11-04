//! Constant values of a machine instruction's p-code temporaries, resolving a
//! register-space LOAD / STORE address (ARM `vld1.N {dX[i]}`). A loop-carried
//! address (ARM `VLD2/3/4`) does not fold and takes the opaque path.
//!
//! The def-site collection and the lift must feed the same ops through this in
//! the same order, or a write gets no phi at a join.

use rustc_hash::FxHashMap;

use strider_cfg::MachineInsnAddr;
use strider_cfg::PcodeInsnAddr;
use strider_ir::IntBinaryOp;
use strider_ir::UBig;
use strider_ir::node::ValueType;

/// `(offset, size)`: temporaries can share an offset at different widths.
type TempKey = (u64, u32);

#[derive(Default)]
pub(crate) struct PcodeConsts {
    /// Values are cleared when the machine instruction changes.
    at: Option<MachineInsnAddr>,
    vals: FxHashMap<TempKey, u128>,
}

impl PcodeConsts {
    /// Start of a region. Must match `collect_def_sites`, which starts fresh
    /// per region even mid-instruction.
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
        // Fold before invalidating, for `t = t + 1`.
        let folded = self.folded(insn);
        // Drop every overlapped entry, not only the same (offset, size).
        self.vals.retain(|&(addr_off, size), _| {
            let entry = rsleigh::Vn {
                addr_space: rsleigh::VnSpace::UNIQUE,
                addr_off,
                size,
            };
            !vn_container::vn_overlaps(out, &entry)
        });
        if let Some(v) = folded {
            self.vals
                .insert((out.addr_off, out.size), mask_to(v, out.size));
        }
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
        let (op, l, r) = match insn.opcode {
            Opcode::Copy => return a(),
            Opcode::IntAdd => (IntBinaryOp::Add, a()?, b()?),
            // `a - b` is `a + (-b)` in the output's ring.
            Opcode::IntSub => (IntBinaryOp::Add, a()?, b()?.wrapping_neg()),
            Opcode::IntMul => (IntBinaryOp::Mul, a()?, b()?),
            Opcode::IntLeft => (IntBinaryOp::ShiftLeft, a()?, b()?),
            Opcode::IntRight => (IntBinaryOp::ShiftRight, a()?, b()?),
            Opcode::IntOr => (IntBinaryOp::Or, a()?, b()?),
            Opcode::IntAnd => (IntBinaryOp::And, a()?, b()?),
            Opcode::IntXor => (IntBinaryOp::Xor, a()?, b()?),
            // Unmodelled: unknown, and the caller fails closed.
            _ => return None,
        };
        // Values are `u128`, and `IntSub` negates in that ring.
        let ty = ValueType::int_from_bytes(insn.output.as_ref()?.size).ok()?;
        if ty.bit_width() > 128 {
            return None;
        }
        let (l, r) = (UBig::from(l), UBig::from(r));
        u128::try_from(&strider_ir::eval::int_binary(op, &l, &r, ty)?).ok()
    }
}

fn mask_to(v: u128, size_bytes: u32) -> u128 {
    v & strider_ir::eval::mask_below(size_bytes.saturating_mul(8))
}

impl<R: rsleigh::MemReader> super::Lifter<R> {
    /// The register slice a register-space STORE writes or LOAD reads, at the
    /// access width, when its address folds into one declared register (a
    /// slot straddling two would alias neither); `None` otherwise.
    pub(crate) fn register_slot_of(
        &self,
        insn: &rsleigh::Insn,
        consts: &PcodeConsts,
    ) -> Option<rsleigh::Vn> {
        let size = match insn.opcode {
            rsleigh::Opcode::Store => insn.inputs.get(2)?.size,
            rsleigh::Opcode::Load => insn.output.as_ref()?.size,
            _ => return None,
        };
        if !self.is_register_space_access(insn) {
            return None;
        }
        let off = u64::try_from(consts.value_of(insn.inputs.get(1)?)?).ok()?;
        let slot = rsleigh::Vn {
            addr_space: rsleigh::VnSpace::REGISTER,
            addr_off: off,
            size,
        };
        vn_container::smallest_enclosing(&self.tables().declared_reg_vns, &slot).map(|_| slot)
    }

    /// Whether `insn` addresses the REGISTER space, resolved or not.
    pub(crate) fn is_register_space_access(&self, insn: &rsleigh::Insn) -> bool {
        self.decode_space_id(insn)
            .is_ok_and(|space| space == rsleigh::VnSpace::REGISTER)
    }
}

/// The tracked varnodes an unresolved register-space write may reach.
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

    /// The write's end is exact at the top of the space: a saturated end
    /// would leave the byte at `u64::MAX` readable under a write over it.
    #[test]
    fn a_write_at_the_top_of_the_space_invalidates_what_it_overlaps() {
        let top = vn(VnSpace::UNIQUE, u64::MAX, 1);
        let wide = vn(VnSpace::UNIQUE, u64::MAX, 4);
        let mut c = PcodeConsts::default();
        c.observe(at(0x10, 0), &op(Opcode::Copy, Some(top), &[konst_of(7, 1)]));
        assert_eq!(c.value_of(&top), Some(7));
        c.observe(at(0x10, 1), &op(Opcode::Popcount, Some(wide), &[top]));
        assert_eq!(c.value_of(&top), None);
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

    /// A 32-byte temporary has no `u128` ring: `2^127 + 2^127` is `2^128`
    /// there, which the carrier wraps to 0. Unknown, not a wrong constant.
    /// The operand arrives by a COPY, which folds at any size, so only the add
    /// can decline.
    #[test]
    fn a_sum_past_the_u128_carrier_is_unknown() {
        let narrow = vn(VnSpace::UNIQUE, 2000, 16);
        let t0 = vn(VnSpace::UNIQUE, 1000, 32);
        let t1 = vn(VnSpace::UNIQUE, 1032, 32);
        let mut c = PcodeConsts::default();
        c.observe(
            at(0x10, 0),
            &op(Opcode::IntLeft, Some(narrow), &[konst(1), konst(127)]),
        );
        c.observe(at(0x10, 1), &op(Opcode::Copy, Some(t0), &[narrow]));
        assert_eq!(c.value_of(&t0), Some(1 << 127));
        c.observe(at(0x10, 2), &op(Opcode::IntAdd, Some(t1), &[t0, t0]));
        assert_eq!(c.value_of(&t1), None);
    }

    /// `0 - 1` at a 32-byte temporary is `2^256 - 1`, which no `u128` holds.
    /// The subtrahend is negated in the table's ring, so without the width
    /// bound the fold would answer the 128-bit all-ones instead.
    #[test]
    fn a_wide_difference_is_unknown_rather_than_negated_at_128_bits() {
        let t = vn(VnSpace::UNIQUE, 1000, 32);
        let mut c = PcodeConsts::default();
        c.observe(
            at(0x10, 0),
            &op(Opcode::IntSub, Some(t), &[konst_of(0, 32), konst_of(1, 32)]),
        );
        assert_eq!(c.value_of(&t), None);
    }

    /// An 11-byte temporary has no value type, so its sum is unknown rather
    /// than computed in a width the IR cannot carry.
    #[test]
    fn a_temporary_of_an_unlisted_width_is_unknown() {
        let t = vn(VnSpace::UNIQUE, 1000, 11);
        let mut c = PcodeConsts::default();
        c.observe(
            at(0x10, 0),
            &op(Opcode::IntAdd, Some(t), &[konst_of(2, 11), konst_of(3, 11)]),
        );
        assert_eq!(c.value_of(&t), None);
    }

    #[test]
    fn a_register_is_never_a_constant() {
        let r = vn(VnSpace::REGISTER, 32, 4);
        let c = PcodeConsts::default();
        assert_eq!(c.value_of(&r), None);
    }
}
