use petgraph::graph::NodeIndex;

mod decode;

use decode::next_pcode_addr;
pub(super) use decode::{DecodeFailure, decode_failure};

use super::Builder;
use crate::types::{PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};
use anyhow::{anyhow, bail};

use crate::Result;

pub(super) fn is_call(insn: &RegionInstruction) -> bool {
    matches!(
        insn.insn.opcode,
        rsleigh::Opcode::Call | rsleigh::Opcode::CallIndirect
    )
}

fn branch_target_operand(insn: &rsleigh::Insn, addr: PcodeInsnAddr) -> Result<rsleigh::Vn> {
    insn.inputs
        .first()
        .copied()
        .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsnOutcome {
    RegionClosed,
    Continue,
}

/// Builds one [`Region`], decoding pcode instructions one at a time.
pub(super) struct RegionBuilder<'b, 'a: 'b, R: rsleigh::MemReader> {
    builder: &'b mut Builder<'a, R>,
    start_addr: PcodeInsnAddr,
    insns: Vec<RegionInstruction>,
    /// Span of a region that seals with no instruction: the zero-p-code-op
    /// instruction at `start_addr`.
    empty_span_len: u32,
    /// `None` only for the function entry region.
    parent_edge: Option<NodeIndex>,
    /// The ISA mode this region decodes in, when the arch has one. Re-imposed
    /// per instruction, since a change point inside the region ends the
    /// entry pin.
    isa_mode: Option<u32>,
    /// The machine instruction this region decoded last.
    last_lift: Option<u64>,
    /// `inst_next` of `last_lift`, already re-derived by
    /// [`Self::rederive_flow`]: the only address the next decode may be at.
    expected_next: Option<u64>,
}

impl<'b, 'a: 'b, R: rsleigh::MemReader> RegionBuilder<'b, 'a, R> {
    pub(super) fn new(
        builder: &'b mut Builder<'a, R>,
        start_addr: PcodeInsnAddr,
        parent_edge: Option<NodeIndex>,
        isa_mode: Option<u32>,
    ) -> Self {
        RegionBuilder {
            builder,
            start_addr,
            insns: Vec::new(),
            empty_span_len: 0,
            parent_edge,
            isa_mode,
            last_lift: None,
            expected_next: None,
        }
    }

    /// Enqueues `addr` as a direct successor of `region`, carrying what this
    /// region decoded in: its ISA mode and its live flowing commits.
    fn enqueue(&mut self, region: NodeIndex, addr: PcodeInsnAddr) -> Result<()> {
        let flowing = self.flowing_commits()?;
        self.builder
            .enqueue(Some(region), addr, self.isa_mode, &flowing);
        Ok(())
    }

    /// A CONST-space target is a signed p-code index offset within the same
    /// machine instruction; a default-code-space target is an absolute
    /// address. Anything else, or an index out of range, is an error.
    fn decode_branch_target(
        &self,
        branch_target_var: rsleigh::Vn,
        branch_insn_addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<PcodeInsnAddr> {
        let default_code_space = self.builder.engine.sleigh.default_code_space();

        match branch_target_var.addr_space {
            rsleigh::VnSpace::CONST => {
                // Sign-extended from the declared width, any of 1..=8 bytes.
                let raw = branch_target_var.addr_off;
                let bits = branch_target_var.size.saturating_mul(8);
                if bits == 0 || bits > 64 {
                    bail!(
                        "unsupported branch-target varnode size {} at opcode {branch_insn_addr:?}",
                        branch_target_var.size
                    );
                }
                let shift = 64 - bits;
                let off = (raw << shift).cast_signed() >> shift;
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or_else(
                    || anyhow!("invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"),
                )?;
                let pcode_count = lift_res.insns.len() as u64;
                // `pcode_count` itself is the fall-through to the next
                // instruction (MIPS `teq` & co skip their `trap` so).
                if target == pcode_count {
                    return next_pcode_addr(
                        PcodeInsnAddr {
                            machine_addr: branch_insn_addr.machine_addr,
                            insn_index: pcode_count.saturating_sub(1),
                        },
                        lift_res,
                    );
                }
                if target > pcode_count {
                    bail!(
                        "invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"
                    );
                }
                Ok(PcodeInsnAddr {
                    machine_addr: branch_insn_addr.machine_addr,
                    insn_index: target,
                })
            }
            // The full `off`, whatever the declared `size`.
            space if space == default_code_space => {
                Ok(PcodeInsnAddr::at_machine_start(branch_target_var.addr_off))
            }
            _ => Err(anyhow!(
                "invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"
            )),
        }
    }

    /// Whether `branch_target_addr` lies outside the function's bound and
    /// fragments.
    fn is_branch_tail_call(&self, branch_target_addr: PcodeInsnAddr) -> bool {
        crate::is_addr_tail_call(
            branch_target_addr.machine(),
            self.builder.start_addr,
            self.builder.options.bound,
            &self.builder.options.fragment_ranges,
        )
    }

    /// Whether falling through from `from` to `to` leaves the function.
    fn is_fall_through_past_end(&self, from: PcodeInsnAddr, to: PcodeInsnAddr) -> bool {
        crate::is_fall_through_past_end(
            from.machine(),
            to.machine(),
            self.builder.start_addr,
            self.builder.options.bound,
            &self.builder.options.fragment_ranges,
        )
    }

    fn process_new_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        self.insns.push(RegionInstruction {
            addr,
            insn: insn.clone(),
            len: lift_res.machine_insn_len as u32,
        });
        match insn.opcode {
            rsleigh::Opcode::Branch => self.process_branch(insn, addr, lift_res),
            rsleigh::Opcode::CondBranch => self.process_cond_branch(insn, addr, lift_res),
            rsleigh::Opcode::Return => {
                self.finish_current_region(RegionTerminator::Return)?;
                Ok(InsnOutcome::RegionClosed)
            }
            rsleigh::Opcode::BranchIndirect => self.process_branch_indirect(insn, addr),
            rsleigh::Opcode::CallOther => self.process_call_other(insn),
            rsleigh::Opcode::Call => self.process_call(insn, addr, lift_res),
            // No target address to look up; only the return address matters.
            rsleigh::Opcode::CallIndirect => {
                self.end_at_call_returning_past_end(addr, next_pcode_addr(addr, lift_res)?)
            }
            _ => Ok(InsnOutcome::Continue),
        }
    }

    /// Ends the region `NoReturn` when the target is in
    /// [`crate::CfgOptions::no_return_targets`], or when the return address is
    /// out of the bound (reported as [`crate::Reason::PastFunctionEnd`]).
    fn process_call(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_no_return =
            crate::direct_call_target(insn, self.builder.engine.sleigh.default_code_space())
                .is_some_and(|target| {
                    let opts = &self.builder.options;
                    opts.local_entries.contains(opts.no_return_targets, target)
                });
        if target_no_return {
            self.finish_current_region(RegionTerminator::NoReturn)?;
            return Ok(InsnOutcome::RegionClosed);
        }
        self.end_at_call_returning_past_end(addr, next_pcode_addr(addr, lift_res)?)
    }

    /// Reports an out-of-range `target` that is the end of the instruction at
    /// `addr` as [`crate::Reason::PastFunctionEnd`]: Sleigh skips a
    /// conditional instruction (ARM, Thumb IT, MIPS branch-likely, x86 `rep`)
    /// with `if (!cc) goto inst_next`, so that branch is its fall-through.
    fn report_skip_past_end(
        &mut self,
        target: PcodeInsnAddr,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) {
        let own_end = addr.machine().checked_add(lift_res.machine_insn_len as u64);
        if target.insn_index == 0 && own_end == Some(target.machine()) {
            self.builder
                .report
                .incomplete
                .insert(target, crate::Reason::PastFunctionEnd);
        }
    }

    /// Ends the region `NoReturn` at a call whose return address `ret` is
    /// out of range, reporting `ret` as [`crate::Reason::PastFunctionEnd`].
    fn end_at_call_returning_past_end(
        &mut self,
        addr: PcodeInsnAddr,
        ret: PcodeInsnAddr,
    ) -> Result<InsnOutcome> {
        if !self.is_fall_through_past_end(addr, ret) {
            return Ok(InsnOutcome::Continue);
        }
        self.finish_current_region(RegionTerminator::NoReturn)?;
        self.builder
            .report
            .incomplete
            .insert(ret, crate::Reason::PastFunctionEnd);
        Ok(InsnOutcome::RegionClosed)
    }

    fn process_branch(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_var = branch_target_operand(insn, addr)?;
        let target = self.decode_branch_target(target_var, addr, lift_res)?;
        if self.is_branch_tail_call(target) {
            self.finish_current_region(RegionTerminator::TailCall {
                target: target.machine().into(),
            })?;
        } else {
            let region = self.finish_current_region(RegionTerminator::Unconditional)?;
            self.enqueue(region, target)?;
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// An out-of-bounds successor edges to a stub (`Builder::edge_to_stub`),
    /// never decoded; the conditional survives.
    fn process_cond_branch(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_var = branch_target_operand(insn, addr)?;
        let target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
        let next_insn_addr = next_pcode_addr(addr, lift_res)?;

        let own_end = addr.machine().checked_add(lift_res.machine_insn_len as u64);
        let true_oob = if target_addr.insn_index == 0 && own_end == Some(target_addr.machine()) {
            self.is_fall_through_past_end(addr, target_addr)
        } else {
            self.is_branch_tail_call(target_addr)
        };
        let false_oob = self.is_fall_through_past_end(addr, next_insn_addr);

        let region = self.finish_current_region(RegionTerminator::CondBranch {
            true_target: target_addr,
        })?;
        // Both arms at one OOB address give two parallel edges to one stub.
        for (oob, successor) in [(true_oob, target_addr), (false_oob, next_insn_addr)] {
            if oob {
                self.report_skip_past_end(successor, addr, lift_res);
                self.builder.edge_to_stub(region, successor)?;
            } else {
                self.enqueue(region, successor)?;
            }
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// Ends the region `NoReturn` when the user op does not return: per the
    /// caller's override for its name, else the target's ABI table or a
    /// PowerPC trap whose TO mask covers every relation.
    fn process_call_other(&mut self, insn: &rsleigh::Insn) -> Result<InsnOutcome> {
        let name = crate::user_op_of(insn, &self.builder.engine.tables.user_op_names)
            .map(|(_, name)| name);
        let overridden = name.and_then(|n| self.builder.options.call_other_overrides.get(n));
        let terminates = match overridden {
            Some(lookup) => lookup.is_no_return(),
            None => {
                let preset = self.builder.engine.arch.preset();
                let class = name.and_then(|n| strider_target::call_other_abi::classify(preset, n));
                // A narrower TO mask is a conditional check with a live
                // fall-through.
                let to_mask = insn
                    .inputs
                    .get(1)
                    .filter(|v| v.addr_space == rsleigh::VnSpace::CONST)
                    .map(|v| u128::from(v.addr_off));
                class.is_some_and(|c| c.is_no_return())
                    || name.is_some_and(|n| {
                        strider_target::call_other_abi::trap_is_unconditional(n, to_mask)
                    })
            }
        };
        if terminates {
            self.finish_current_region(RegionTerminator::NoReturn)?;
            return Ok(InsnOutcome::RegionClosed);
        }
        Ok(InsnOutcome::Continue)
    }

    /// Seats a terminator from [`crate::CfgOptions::seated`] at this site:
    /// `LinkRegister` as `Return`, an out-of-range `Single` as `TailCall`,
    /// anything else as a `Switch`. No answer, or one out-of-range or
    /// off-boundary arm, defers the whole site as `UnresolvedIndirectBranch`.
    fn process_branch_indirect(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
    ) -> Result<InsnOutcome> {
        let target_vn = branch_target_operand(insn, addr)?;
        // Before another region can repaint this address; deferred sites too.
        let flowing_isa = self.builder.sample_flowing_isa_bit(addr);
        let arms = match self.builder.options.seated(addr) {
            None | Some(crate::ResolvedTargets::Withdrawn) => {
                self.finish_current_region(RegionTerminator::defer(target_vn, addr))?;
                return Ok(InsnOutcome::RegionClosed);
            }
            Some(crate::ResolvedTargets::LinkRegister) => {
                self.builder.report.link_register_seated.push(addr);
                self.finish_current_region(RegionTerminator::Return)?;
                return Ok(InsnOutcome::RegionClosed);
            }
            Some(&crate::ResolvedTargets::Single(t))
                if self.is_branch_tail_call(PcodeInsnAddr::at_machine_start(t.addr)) =>
            {
                self.builder.report.tail_call_seated.push(addr);
                self.finish_current_region(RegionTerminator::TailCall { target: t })?;
                return Ok(InsnOutcome::RegionClosed);
            }
            // An in-function `Single` is a one-arm `Switch`, which keeps the
            // selector so a later round can widen it.
            Some(seated) => seated.arms(),
        };
        // A bad arm means the table bound is wrong, so the whole site defers.
        let mut deferred = false;
        for target in arms {
            let a = PcodeInsnAddr::at_machine_start(target.addr);
            if self.builder.addr_is_interior_non_boundary(a) {
                self.builder
                    .report
                    .incomplete
                    .insert(a, crate::Reason::InteriorTarget);
                deferred = true;
            } else if self.is_branch_tail_call(a) {
                deferred = true;
            }
        }
        if deferred {
            self.finish_current_region(RegionTerminator::defer(target_vn, addr))?;
            return Ok(InsnOutcome::RegionClosed);
        }
        let region = self.finish_current_region(RegionTerminator::Switch {
            target_vn,
            targets: arms.to_vec(),
            addr,
        })?;
        // Enqueued highest first so the LIFO explores lowest first: a higher
        // over-read arm then lands inside an existing region and is dropped.
        let flowing = self.flowing_commits()?;
        for target in arms.iter().rev() {
            self.builder.enqueue_resolved(
                region,
                PcodeInsnAddr::at_machine_start(target.addr),
                target.isa_bit.or(flowing_isa),
                &flowing,
            );
        }
        Ok(InsnOutcome::RegionClosed)
    }

    fn finish_current_region(&mut self, terminator: RegionTerminator) -> Result<NodeIndex> {
        let mut insns = std::mem::take(&mut self.insns);
        insns.shrink_to_fit();
        let region = self.builder.add_region(Region {
            start_addr: self.start_addr,
            empty_span_len: self.empty_span_len,
            insns,
            terminator,
            isa_mode: self.isa_mode,
        })?;
        if let Some(parent_id) = self.parent_edge {
            self.builder.region_graph.add_edge(parent_id, region, ());
        }
        Ok(region)
    }

    /// Seals the region `Unconditional` onto `existing`, the region already
    /// starting at `addr`, reporting an ISA-mode clash there.
    fn seal_onto(&mut self, addr: PcodeInsnAddr, existing: NodeIndex) -> Result<()> {
        self.builder
            .note_isa_mode_clash(addr, existing, self.isa_mode);
        let region = self.finish_current_region(RegionTerminator::Unconditional)?;
        self.builder.region_graph.add_edge(region, existing, ());
        Ok(())
    }

    /// Seals onto the region already starting at `addr`, if any; else
    /// processes `insn`.
    fn process_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        if let Some(existing) = self.builder.index.get(addr) {
            self.seal_onto(addr, existing)?;
            return Ok(InsnOutcome::RegionClosed);
        }
        self.process_new_insn(insn, addr, lift_res)
    }

    /// Decodes instructions in order until the region closes: an instruction
    /// can commit context (ARM IT state, Thumb mode) at a later address. The
    /// first may start mid-p-code (`start_addr.insn_index > 0`).
    pub(super) fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            debug_assert!(
                !self.is_branch_tail_call(cur_addr),
                "decoding {cur_addr:?} outside the function bound"
            );
            let lift_res = match self.lift_code(cur_addr.machine()) {
                Ok(lift_res) => lift_res,
                Err(e) if cur_addr.machine_addr != self.start_addr.machine_addr => {
                    // Off a seeded arm, falling into bytes that are not code
                    // drops the arm unless a call before them seals it.
                    let off_arm = self.builder.descent.is_some();
                    let here = decode_failure(&e).filter(|f| f.addr() == cur_addr.machine());
                    let (marked_data, reason) = match here {
                        Some(DecodeFailure::Unmapped(_)) if !off_arm => {
                            return self.seal_and_report(cur_addr, crate::Reason::UnmappedTarget);
                        }
                        Some(DecodeFailure::Unmapped(_)) => (false, crate::Reason::UnmappedTarget),
                        Some(DecodeFailure::NotCode { marked_data, .. }) => {
                            (marked_data, crate::Reason::UndecodableTarget)
                        }
                        _ => return Err(e),
                    };
                    if self.seal_at_call_not_returning(cur_addr, reason)? {
                        return Ok(());
                    }
                    // A zero-pcode-op run loses no code, nor does code a call
                    // falls into through one. `Builder::finish` settles
                    // whether a call falls into the region and so whether `e`
                    // fails the function.
                    if (self.insns.is_empty() && !off_arm)
                        || !self.builder.calls_falling_into(self.parent_edge).is_empty()
                    {
                        self.seal_and_report(cur_addr, reason)?;
                        let sealed = self
                            .builder
                            .index
                            .get(self.start_addr)
                            .expect("just sealed");
                        let fatal = (!marked_data).then_some(e);
                        self.builder.sealed_into_no_code.push((sealed, fatal));
                        return Ok(());
                    }
                    if marked_data && !off_arm {
                        return self.seal_and_report(cur_addr, reason);
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            // A p-code index; fits a usize.
            #[allow(clippy::cast_possible_truncation)]
            let start_pcode_idx = cur_addr.insn_index as usize;
            let is_zero_op = lift_res.insns.len() <= start_pcode_idx;

            if cur_addr.machine_addr != self.start_addr.machine_addr {
                self.note_empty_span_to(cur_addr);
                // Exact key: a start inside an instruction is stepped over
                // (see `note_stepped_over_region_starts`).
                if let Some(existing) = self.builder.index.get(cur_addr) {
                    return self.seal_onto(cur_addr, existing);
                }
                // Every zero-p-code-op instruction is its own empty region, so
                // a non-empty region starts at `insns[0].addr`.
                if self.insns.is_empty() || is_zero_op {
                    let region = self.finish_current_region(RegionTerminator::Unconditional)?;
                    self.enqueue(region, cur_addr)?;
                    return Ok(());
                }
            }

            self.note_stepped_over_region_starts(cur_addr, &lift_res);

            for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
                cur_addr.insn_index = i as u64;
                let res = self.process_insn(insn, cur_addr, &lift_res)?;
                if res == InsnOutcome::RegionClosed {
                    return Ok(());
                }
            }
            let from = cur_addr;
            cur_addr = next_pcode_addr(cur_addr, &lift_res)?;
            if self.seal_past_function_end(from, cur_addr)? {
                return Ok(());
            }
        }
    }

    /// Queues every region start strictly inside the instruction at
    /// `cur_addr` as a [`Builder::interior_candidates`] entry: those bytes get
    /// two owners. A [`Builder::edge_to_stub`] stub owns no bytes and is
    /// skipped.
    fn note_stepped_over_region_starts(
        &mut self,
        cur_addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) {
        let base = cur_addr.machine();
        let len = lift_res.machine_insn_len as u64;
        let (Some(lo), Some(hi)) = (base.checked_add(1), base.checked_add(len)) else {
            return;
        };
        if lo >= hi {
            return;
        }
        let range = PcodeInsnAddr::at_machine_start(lo)..PcodeInsnAddr::at_machine_start(hi);
        let builder = &mut *self.builder;
        for (start, region) in builder.index.starts_in(range) {
            let r = &builder.region_graph[region];
            if !(r.insns.is_empty() && matches!(r.terminator, RegionTerminator::TailCall { .. })) {
                builder.interior_candidates.push((base, start));
            }
        }
    }

    /// Seals the region onto a [`Builder::edge_to_stub`] stub at `cur_addr`,
    /// where the fall-through found nothing to decode, reporting it as
    /// `reason`.
    fn seal_and_report(&mut self, cur_addr: PcodeInsnAddr, reason: crate::Reason) -> Result<()> {
        self.note_empty_span_to(cur_addr);
        let region = self.finish_current_region(RegionTerminator::Unconditional)?;
        self.builder.edge_to_stub(region, cur_addr)?;
        self.builder.report.incomplete.insert(cur_addr, reason);
        Ok(())
    }

    /// Records the bytes behind `cur_addr` as an empty region's span; a no-op
    /// once anything has been decoded.
    fn note_empty_span_to(&mut self, cur_addr: PcodeInsnAddr) {
        if self.insns.is_empty() {
            self.empty_span_len =
                u32::try_from(cur_addr.machine().saturating_sub(self.start_addr.machine()))
                    .unwrap_or(0);
        }
    }

    /// Ends the region `NoReturn` at its last call, dropping what follows,
    /// when the fall-through reached `cur_addr`, which holds no code (left by
    /// an unmarked no-return callee). Reports `cur_addr` as `reason`; returns
    /// whether the region held a call.
    fn seal_at_call_not_returning(
        &mut self,
        cur_addr: PcodeInsnAddr,
        reason: crate::Reason,
    ) -> Result<bool> {
        let Some(call) = self.insns.iter().rposition(is_call) else {
            return Ok(false);
        };
        self.insns.truncate(call + 1);
        self.finish_current_region(RegionTerminator::NoReturn)?;
        self.builder.report.incomplete.insert(cur_addr, reason);
        Ok(true)
    }

    /// Seals onto a stub at `cur_addr` when the fall-through from `from` ran
    /// past a `Size` or fragment end, reporting
    /// [`crate::Reason::PastFunctionEnd`]; returns whether it sealed. Off a
    /// seeded arm it errs with [`DecodeFailure::PastEnd`] instead.
    fn seal_past_function_end(
        &mut self,
        from: PcodeInsnAddr,
        cur_addr: PcodeInsnAddr,
    ) -> Result<bool> {
        if !self.is_fall_through_past_end(from, cur_addr) {
            return Ok(false);
        }
        if self.builder.descent.is_some() {
            return Err(DecodeFailure::PastEnd(cur_addr).into());
        }
        self.seal_and_report(cur_addr, crate::Reason::PastFunctionEnd)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_sign_loss)]

    use rsleigh::mem_readers::BufMemReader;
    use rsleigh::{Vn, VnSpace};
    use strider_target::SleighArch;

    use super::super::WorkItem;
    use super::*;
    use crate::CfgOptions;
    use crate::test_support::addr as addr_at;
    use crate::test_support::*;

    fn fake_lift_res(n: usize) -> rsleigh::LiftRes {
        fake_lift_res_with_len(n, 1)
    }

    fn fake_lift_res_with_len(n: usize, machine_insn_len: usize) -> rsleigh::LiftRes {
        rsleigh::LiftRes {
            insns: (0..n).map(|_| fake_insn()).collect(),
            machine_insn_len,
        }
    }

    /// `process_insn`'s seal fires when a pcode index inside the current
    /// machine instruction already starts a region, and it wires the edge
    /// itself, so `Builder::explore`'s clash check never sees it.
    #[test]
    fn a_pcode_index_seal_onto_a_differently_decoded_region_reports_the_clash() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let existing = b
            .add_region(make_region(&[(0x1000, 3)]))
            .expect("add_region");
        b.region_graph[existing].isa_mode = Some(1);
        let mut rb = RegionBuilder::new(&mut b, addr_at(0x1000, 0), None, Some(0));
        let lift = fake_lift_res(4);

        let outcome = rb
            .process_insn(&fake_insn(), addr_at(0x1000, 3), &lift)
            .expect("process_insn");

        assert_eq!(outcome, InsnOutcome::RegionClosed);
        assert_eq!(
            b.report
                .incomplete
                .of(crate::Reason::IsaModeConflict)
                .collect::<Vec<_>>(),
            vec![addr_at(0x1000, 3)],
            "a seal onto a region decoded in the other mode is a clash"
        );
    }

    fn make_region_builder<'b, 'a: 'b>(
        b: &'b mut Builder<'a, TestReader>,
        start: PcodeInsnAddr,
    ) -> RegionBuilder<'b, 'a, TestReader> {
        RegionBuilder::new(b, start, None, None)
    }

    /// Whether the MIPS instruction encoded by `word` branches to exactly its
    /// own pcode count, the "leave this pcode block" idiom
    /// [`RegionBuilder::decode_branch_target`] resolves to the next machine
    /// instruction.
    fn branches_to_pcode_count(word: u32) -> bool {
        let arch = SleighArch::mipsbe32();
        let reader = BufMemReader::new(word.to_be_bytes().to_vec(), 0x1000);
        let mut sleigh = arch.sleigh(reader).expect("create Sleigh");
        let lift = sleigh.lift_one(0x1000).expect("lift_one");
        let pcode_count = lift.insns.len() as i64;
        lift.insns.iter().enumerate().any(|(i, insn)| {
            matches!(
                insn.opcode,
                rsleigh::Opcode::Branch | rsleigh::Opcode::CondBranch
            ) && insn.inputs[0].addr_space == VnSpace::CONST
                && i as i64 + insn.inputs[0].addr_off as i64 == pcode_count
        })
    }

    /// The conditional traps guard their `trap` with a forward branch past the
    /// end of their own pcode sequence; the arithmetic they are often confused
    /// with emits no branch at all.
    #[test]
    fn mips_conditional_traps_branch_to_their_pcode_count() {
        // SPECIAL rs=$a0 rt=$a1: teq funct=0x34, tne funct=0x36.
        assert!(branches_to_pcode_count(0x0085_0034), "teq");
        assert!(branches_to_pcode_count(0x0085_0036), "tne");
        // SPECIAL rs=$a0 rt=$a1: div funct=0x1a, slt rd=$v0 funct=0x2a.
        assert!(!branches_to_pcode_count(0x0085_001a), "div");
        assert!(!branches_to_pcode_count(0x0085_102a), "slt");
    }

    /// A `Call` whose target varnode is not in code space: its offset is a
    /// register number, and reading it as an address can match a no-return
    /// target.
    #[test]
    fn a_call_target_outside_code_space_is_not_read_as_an_address() {
        let no_return: rustc_hash::FxHashSet<u64> = [0x10].into_iter().collect();
        let opts = CfgOptions {
            no_return_targets: &no_return,
            ..CfgOptions::default()
        };
        let mut engine = make_engine();
        let mut b = make_builder_opts(0x1000, &mut engine, &opts);
        let mut rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let insn = rsleigh::Insn {
            opcode: rsleigh::Opcode::Call,
            output: None,
            inputs: vec![Vn {
                addr_off: 0x10,
                addr_space: VnSpace::REGISTER,
                size: 8,
            }]
            .into(),
        };

        let outcome = rb
            .process_call(&insn, addr_at(0x1000, 0), &fake_lift_res(1))
            .expect("process_call");

        assert_eq!(outcome, InsnOutcome::Continue);
    }

    /// A CONST branch offset counts p-code ops from the current one,
    /// sign-extended from the varnode's declared width: a 32-bit-encoded -4
    /// read as 4_294_967_292 fails the whole function's lift.
    #[test]
    fn const_space_offset_is_sign_extended_from_its_declared_width() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x2000, 0));
        let lift = fake_lift_res(8);
        // A non-power-of-two width included.
        for (size, addr_off, to) in [
            (1, 0xFC, 2),
            (3, 0xFF_FFFC, 2),
            (4, 0xFFFF_FFFC, 2),
            (8, 0xFFFF_FFFF_FFFF_FFFC, 2),
            (8, 0, 6),
            (8, 1, 7),
        ] {
            let vn = Vn {
                addr_off,
                addr_space: VnSpace::CONST,
                size,
            };
            let target = rb
                .decode_branch_target(vn, addr_at(0x2000, 6), &lift)
                .expect("a CONST branch inside the instruction must decode");
            assert_eq!(target, addr_at(0x2000, to), "size {size}, {addr_off:#x}");
        }
    }

    #[test]
    fn default_code_space_is_absolute_machine_address() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let vn = Vn {
            addr_off: 0xabc0,
            addr_space: b.engine.sleigh.default_code_space(),
            size: 8,
        };
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let lift = fake_lift_res(1);
        let target = rb
            .decode_branch_target(vn, addr_at(0x1000, 4), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0xabc0, 0));
    }

    /// A non-code space, and a CONST offset before the instruction's first
    /// p-code op or past its fall-through.
    #[test]
    fn an_unaddressable_target_is_an_invalid_branch_target() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let lift = fake_lift_res(4);
        for (addr_space, addr_off, at) in [
            (VnSpace::REGISTER, 0x10, 0),
            (VnSpace::new(b'x'), 0x2000, 0),
            (VnSpace::UNIQUE, 0x40, 0),
            (VnSpace::CONST, (-5_i64) as u64, 2),
            (VnSpace::CONST, 5, 0),
        ] {
            let vn = Vn {
                addr_off,
                addr_space,
                size: 8,
            };
            let err = rb
                .decode_branch_target(vn, addr_at(0x1000, at), &lift)
                .unwrap_err();
            assert!(
                err.to_string().contains("invalid branch target variable"),
                "{addr_space:?} {addr_off:#x}: {err}"
            );
        }
    }

    #[test]
    fn const_space_branch_to_pcode_count_falls_through_to_next_insn() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let pcode_count = 4usize;
        let lift = fake_lift_res_with_len(pcode_count, 4);
        let vn = Vn {
            addr_off: pcode_count as u64,
            addr_space: VnSpace::CONST,
            size: 8,
        };
        let target = rb
            .decode_branch_target(vn, addr_at(0x1000, 0), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0x1004, 0));
    }

    #[test]
    fn next_pcode_addr_machine_address_overflow_errors() {
        let lift = fake_lift_res_with_len(1, 16);
        let cur = addr_at(u64::MAX - 8, 0);
        let err = next_pcode_addr(cur, &lift).unwrap_err();
        assert!(
            err.to_string().contains("machine-address overflow"),
            "got: {err}"
        );
    }

    #[test]
    fn next_pcode_addr_zero_length_machine_insn_errors() {
        // A non-empty pcode body reporting `machine_insn_len == 0` pins
        // `cur_addr` and hangs the build loop forever.
        let lift = fake_lift_res_with_len(1, 0);
        let cur = addr_at(0x1000, 0);
        let err = next_pcode_addr(cur, &lift).unwrap_err();
        assert!(
            err.to_string().contains("zero-length machine instruction"),
            "got: {err}"
        );
    }

    #[test]
    fn next_pcode_addr_non_overflowing_advance_succeeds() {
        let lift = fake_lift_res_with_len(1, 4);
        let cur = addr_at(0x1000, 0);
        let next = next_pcode_addr(cur, &lift).unwrap();
        assert_eq!(next, addr_at(0x1004, 0));
    }

    #[test]
    fn next_pcode_addr_within_machine_insn_advances_pcode_index() {
        let lift = fake_lift_res(4);
        let cur = addr_at(0x1000, 1);
        let next = next_pcode_addr(cur, &lift).unwrap();
        assert_eq!(next, addr_at(0x1000, 2));
    }

    fn lift_at(bytes: Vec<u8>, base: u64, at: u64) -> rsleigh::LiftRes {
        make_sleigh_over(bytes, base)
            .lift_one(at)
            .expect("lift_one")
    }

    fn find_pcode(lift: &rsleigh::LiftRes, want: rsleigh::Opcode) -> (u64, rsleigh::Insn) {
        let (idx, i) = lift
            .insns
            .iter()
            .enumerate()
            .find(|(_, i)| i.opcode == want)
            .unwrap_or_else(|| panic!("no pcode op with opcode {want:?}"));
        (idx as u64, i.clone())
    }

    #[test]
    fn non_terminating_insn_keeps_region_open() {
        let base = 0x1000u64;
        let bytes = vec![0x31u8, 0xc0]; // xor eax, eax
        let lift = lift_at(bytes.clone(), base, base);
        assert!(!lift.insns.is_empty());
        let first = lift.insns[0].clone();
        assert!(!matches!(
            first.opcode,
            rsleigh::Opcode::Branch | rsleigh::Opcode::CondBranch | rsleigh::Opcode::Return
        ));
        let mut engine = make_engine_over(bytes, base);
        let mut b = make_builder(base, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb
            .process_new_insn(&first, addr_at(base, 0), &lift)
            .unwrap();
        assert_eq!(res, InsnOutcome::Continue);
        assert_eq!(rb.insns.len(), 1);
    }

    #[test]
    fn return_ends_region() {
        let base = 0x1000u64;
        let bytes = vec![0xc3u8];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, ret_insn) = find_pcode(&lift, rsleigh::Opcode::Return);
        let mut engine = make_engine_over(bytes, base);
        let mut b = make_builder(base, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb
            .process_new_insn(&ret_insn, addr_at(base, pos), &lift)
            .unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].terminator, RegionTerminator::Return);
    }

    #[test]
    fn branch_indirect_defers_via_unresolved_indirect_branch() {
        // `jmp rax` cannot be proven here without a classification, so it
        // must defer rather than error.
        let base = 0x1000u64;
        let bytes = vec![0xffu8, 0xe0]; // jmp rax
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, indirect) = find_pcode(&lift, rsleigh::Opcode::BranchIndirect);
        let mut engine = make_engine_over(bytes, base);
        let mut b = make_builder(base, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb
            .process_new_insn(&indirect, addr_at(base, pos), &lift)
            .expect("unresolvable BranchIndirect must defer, not error");
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        match &regions[0].terminator {
            RegionTerminator::UnresolvedIndirectBranch { addr, .. } => {
                assert_eq!(addr.machine(), base);
            }
            other => panic!("expected UnresolvedIndirectBranch, got {other:?}"),
        }
    }

    #[test]
    fn cond_branch_finishes_region_and_enqueues_both_cases() {
        let base = 0x1000u64;
        let bytes = vec![0x74u8, 0x00, 0xc3, 0xc3];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, cbr) = find_pcode(&lift, rsleigh::Opcode::CondBranch);
        let mut engine = make_engine_over(bytes, base);
        let mut b = make_builder(base, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb
            .process_new_insn(&cbr, addr_at(base, pos), &lift)
            .unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        // `je +0` at 0x1000 targets 0x1002, which is also the fall-through:
        // the degenerate both-arms-same-address case.
        match regions[0].terminator {
            RegionTerminator::CondBranch { true_target } => {
                assert_eq!(true_target, addr_at(0x1002, 0));
            }
            ref other => panic!("expected CondBranch, got {other:?}"),
        }

        assert_eq!(
            b.work_queue.len(),
            2,
            "CondBranch must enqueue both true and false targets"
        );
        let region_id = b.region_graph.node_indices().next().unwrap();
        for WorkItem { parent, addr, .. } in &b.work_queue {
            assert_eq!(
                *parent,
                Some(region_id),
                "successor wired to the cond-branch region"
            );
            assert_eq!(*addr, addr_at(0x1002, 0));
        }
    }

    #[test]
    fn finish_with_branch_terminator_to_distinct_target() {
        // `jmp +1` targets 0x1003, distinct from the 0x1002 fall-through.
        let base = 0x1000u64;
        let bytes = vec![0xebu8, 0x01, 0xc3];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
        let mut engine = make_engine_over(bytes, base);
        let mut b = make_builder(base, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb
            .process_new_insn(&branch, addr_at(base, pos), &lift)
            .unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].terminator, RegionTerminator::Unconditional);
    }

    #[test]
    fn finish_with_tail_call_terminator_targets_below_start() {
        // `jmp -10` from 0x1000 lands at 0x0ff8, below the function start.
        let base = 0x1000u64;
        #[allow(clippy::cast_sign_loss)]
        let bytes = vec![0xebu8, -10_i8 as u8, 0xc3];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
        let mut engine = make_engine_over(bytes, base);
        let mut b = make_builder(base, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb
            .process_new_insn(&branch, addr_at(base, pos), &lift)
            .unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        assert_eq!(
            b.work_queue.len(),
            0,
            "tail-call must not enqueue successor"
        );
        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        assert_eq!(
            regions[0].terminator,
            RegionTerminator::TailCall {
                target: 0x0ff8.into(),
            }
        );
    }

    #[test]
    fn a_branch_with_no_operand_errors() {
        for opcode in [rsleigh::Opcode::Branch, rsleigh::Opcode::CondBranch] {
            let mut engine = make_engine();
            let mut b = make_builder(0x1000, &mut engine);
            let mut rb = make_region_builder(&mut b, addr_at(0x1000, 0));
            let lift = fake_lift_res(1);
            let bad_insn = rsleigh::Insn {
                opcode,
                inputs: vec![].into(),
                output: None,
            };
            let err = rb
                .process_new_insn(&bad_insn, addr_at(0x1000, 0), &lift)
                .unwrap_err();
            assert!(
                err.to_string().contains("no target operand"),
                "{opcode:?}: {err}"
            );
        }
    }
}
