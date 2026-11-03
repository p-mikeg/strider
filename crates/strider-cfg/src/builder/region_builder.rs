use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

mod decode;

use decode::next_pcode_addr;
pub(super) use decode::{DecodeFailure, decode_failure};

use super::Builder;
use crate::types::{PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};
use anyhow::{anyhow, bail};

use crate::Result;

/// Whether `seal_at_call_not_returning` may end a region here.  Not the only
/// `NoReturn` emitter: see [`RegionTerminator::NoReturn`].
fn is_call(insn: &RegionInstruction) -> bool {
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
    /// `'b` is the borrow of the Builder, scoped to one `build()` call; `'a`
    /// is the longer Sleigh borrow the Builder itself holds.
    builder: &'b mut Builder<'a, R>,
    start_addr: PcodeInsnAddr,
    insns: Vec<RegionInstruction>,
    /// Bytes consumed by the zero-pcode-op instruction at `start_addr`, set
    /// once decoding leaves it. Read only when the region seals with no
    /// instruction, where nothing else records its span.
    empty_span_len: u32,
    /// `None` only for the function entry region.
    parent_edge: Option<NodeIndex>,
    /// The ISA mode this region decodes in, when the arch has one.
    ///
    /// Pinning it once at the region's first address is not enough: Sleigh's
    /// context paint stops at any address a previous write already split, so a
    /// change point INSIDE this region leaves everything past it decoding in
    /// the other region's mode. Re-imposed per instruction, and only written
    /// when a read-back shows it actually drifted, so a region with no barrier
    /// in it costs reads and no parse-cache flush.
    isa_mode: Option<u32>,
    /// The machine instruction this region decoded last, whose commits the
    /// next sequential decode reads.
    last_lift: Option<u64>,
    /// `inst_next` of `last_lift`, whose flow context [`Self::rederive_flow`]
    /// already made this build's own: the one address the next decode may be at.
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

    /// Pcode encodes branch targets two ways: CONST-space is a signed offset
    /// on the pcode index within the *same* machine instruction, and default
    /// code space is an absolute virtual address (pcode index implicitly 0).
    ///
    /// `lift_res.insns.len()` bounds the CONST-space index; the fall-through
    /// idiom (`target == pcode_count`) then reads `machine_insn_len` through
    /// [`next_pcode_addr`].
    fn decode_branch_target(
        &self,
        branch_target_var: rsleigh::Vn,
        branch_insn_addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<PcodeInsnAddr> {
        let default_code_space = self.builder.engine.sleigh.default_code_space();

        match branch_target_var.addr_space {
            // An out-of-range index must NOT slip through: the build loop
            // would silently skip it, advance past the end of this machine
            // instruction's pcode sequence, and emit a wrong CFG with no
            // diagnostic.
            rsleigh::VnSpace::CONST => {
                // Sign-extend from the varnode's declared byte width first.
                // Cast straight from u64 and a 32-bit-encoded -4 (0xFFFFFFFC)
                // reads as 4_294_967_292, and the bounds check below wrongly
                // rejects a valid target.
                // Any width 1..=8, not just the powers of two: a sla is free
                // to declare a 3-byte CONST, and rejecting one failed the whole
                // function's lift.
                let raw = branch_target_var.addr_off;
                let bits = branch_target_var.size.saturating_mul(8);
                if bits == 0 || bits > 64 {
                    bail!(
                        "unsupported branch-target varnode size {} at opcode {branch_insn_addr:?}",
                        branch_target_var.size
                    );
                }
                // Shift the sign bit up to bit 63, then arithmetic-shift back
                // down.
                let shift = 64 - bits;
                let off = (raw << shift).cast_signed() >> shift;
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or_else(
                    || anyhow!("invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"),
                )?;
                let pcode_count = lift_res.insns.len() as u64;
                // Sleigh idiom: branching to exactly `pcode_count` (one past
                // the last pcode insn) means "leave this pcode block, fall
                // through to the next machine instruction".  MIPS `teq` / `tne`
                // / `tge` / `tgeu` / `tlt` / `tltu` emit it to skip their
                // `trap`.  Anything strictly beyond is rejected.
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
            // Sleigh emits an absolute target as the full 64-bit `off`
            // regardless of the varnode's declared `size`, so `size` is
            // deliberately ignored: unlike the CONST arm's signed pcode-index
            // offset, these are unsigned addresses needing no sign extension.
            space if space == default_code_space => {
                Ok(PcodeInsnAddr::at_machine_start(branch_target_var.addr_off))
            }
            _ => Err(anyhow!(
                "invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"
            )),
        }
    }

    /// Whether `branch_target_addr` lies outside the function's bound.
    fn is_branch_tail_call(&self, branch_target_addr: PcodeInsnAddr) -> bool {
        crate::is_addr_tail_call(
            branch_target_addr.machine_addr.addr,
            self.builder.start_addr,
            self.builder.options.bound,
        )
    }

    /// Appends `insn` to the current region, then dispatches on the opcode.
    /// Any opcode without an arm is a non-terminator: decoding continues.
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
            // The target is a register, so no no-return mark applies and
            // `process_call`'s lookup would read a register offset as an
            // address. The return address still decides whether any
            // in-function code follows.
            rsleigh::Opcode::CallIndirect => {
                self.end_at_call_returning_past_end(next_pcode_addr(addr, lift_res)?)
            }
            _ => Ok(InsnOutcome::Continue),
        }
    }

    /// A `call` normally falls through to its return address, so it is not a
    /// terminator.  Two cases end the region as `NoReturn`, which the IR
    /// lifter lowers to `Call + Unreachable`:
    ///
    /// 1. The target is in [`crate::CfgOptions::no_return_targets`]
    ///    (`exit`/`abort`), which ends the region wherever the return address
    ///    lands.
    /// 2. The return address is outside the function's bound, so no
    ///    in-function code follows: an unmarked no-return callee (FreeBSD
    ///    `exit1`), or the function ends at the call. The return address is
    ///    reported as [`crate::Reason::PastFunctionEnd`].
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
        self.end_at_call_returning_past_end(next_pcode_addr(addr, lift_res)?)
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
        let own_end = (addr.machine_addr.addr).checked_add(lift_res.machine_insn_len as u64);
        if target.insn_index == 0 && own_end == Some(target.machine_addr.addr) {
            self.builder
                .incomplete
                .insert(target, crate::Reason::PastFunctionEnd);
        }
    }

    /// Ends the region `NoReturn` at a call whose return address `ret` is
    /// out of range, reporting `ret` as [`crate::Reason::PastFunctionEnd`]:
    /// a callee that returns would fall out of the function.
    fn end_at_call_returning_past_end(&mut self, ret: PcodeInsnAddr) -> Result<InsnOutcome> {
        if !self.is_branch_tail_call(ret) {
            return Ok(InsnOutcome::Continue);
        }
        self.finish_current_region(RegionTerminator::NoReturn)?;
        self.builder
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
            // A direct `b <oob>` stays in the current ISA mode; no switch.
            self.finish_current_region(RegionTerminator::TailCall {
                target: target.machine_addr.addr.into(),
            })?;
        } else {
            let region = self.finish_current_region(RegionTerminator::Unconditional)?;
            self.enqueue(region, target)?;
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// The conditional ALWAYS survives.  An out-of-bounds successor is wired
    /// to a synthetic empty stub (`Builder::edge_to_stub`) rather than
    /// deleted.
    fn process_cond_branch(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_var = branch_target_operand(insn, addr)?;
        let target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
        let next_insn_addr = next_pcode_addr(addr, lift_res)?;

        // Classify before lifting: an OOB successor is never decoded.
        let true_oob = self.is_branch_tail_call(target_addr);
        let false_oob = self.is_branch_tail_call(next_insn_addr);

        // Edges are unweighted; `region_if` recovers polarity by matching each
        // successor region against `true_target`.  A stub owns exactly its
        // start address, which IS the OOB target, so that matching covers
        // stubs unchanged.
        let region = self.finish_current_region(RegionTerminator::CondBranch {
            true_target: target_addr,
        })?;
        // An OOB successor is wired straight to its (shared, possibly
        // pre-existing) stub and never enqueued, so no TARGET outside the
        // function bound is decoded.  Sequential decoding still crosses it by
        // up to one instruction: an instruction starting in range is decoded
        // whole, and its trailing bytes join the region's span.  Nothing looks
        // those bytes up.  An address at or above the bound classifies as a
        // tail call and is never explored.  When both arms hit the same OOB
        // address this adds two parallel edges to one stub, mirroring the
        // in-range degenerate case: `region_if` reads the second edge as the
        // fall-through side.
        for (oob, successor) in [(true_oob, target_addr), (false_oob, next_insn_addr)] {
            if oob {
                // Either arm at this instruction's own end is a fall-through
                // out of the function, reported as one.
                self.report_skip_past_end(successor, addr, lift_res);
                self.builder.edge_to_stub(region, successor)?;
            } else {
                self.enqueue(region, successor)?;
            }
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// Resolves the user-op id from the CONST input at position 0 and
    /// terminates the region when the op does not return.  A caller override
    /// for the name answers that alone; with none, the target ABI table
    /// answers, plus the PowerPC traps whose TO mask covers every relation.  An
    /// unexpected input shape falls through to `Continue` rather than erroring.
    fn process_call_other(&mut self, insn: &rsleigh::Insn) -> Result<InsnOutcome> {
        let name = crate::user_op_of(insn, &self.builder.engine.tables.user_op_names)
            .map(|(_, name)| name);
        let overridden = name.and_then(|n| self.builder.options.call_other_overrides.get(n));
        let terminates = match overridden {
            // A caller override states what this binary's build of the op does,
            // which includes the trap masks the built-in rule below reads.
            Some(lookup) => lookup.is_no_return(),
            None => {
                let preset = self.builder.engine.arch.preset();
                let class = name.and_then(|n| strider_target::call_other_abi::classify(preset, n));
                // A PowerPC trap firing on every relation ends the region: the
                // table classes the family conservatively because a narrower TO
                // mask is a conditional check whose fall-through is live.
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
            // The CallOther is already in `self.insns` from the
            // `process_new_insn` prologue push, so the region carries it.
            // A trailing BranchIndirect is never decoded.
            self.finish_current_region(RegionTerminator::NoReturn)?;
            return Ok(InsnOutcome::RegionClosed);
        }
        Ok(InsnOutcome::Continue)
    }

    /// Seats a terminator from this site's `known_targets` entry: `LinkRegister`
    /// as `Return`, an out-of-range `Single` as `TailCall`, anything else as a
    /// `Switch`, but only when EVERY arm seats.  One out-of-range or
    /// off-boundary arm defers the whole site via `UnresolvedIndirectBranch`,
    /// as does a site with no entry.
    fn process_branch_indirect(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
    ) -> Result<InsnOutcome> {
        let target_vn = branch_target_operand(insn, addr)?;
        // Sampled here, before any other region can repaint this address, and
        // for every site: a placeholder's caller needs the same bit a seated
        // target would have decoded in.
        let flowing_isa = self.builder.sample_flowing_isa_bit(addr);
        let arms = match self.builder.options.seated(addr) {
            // Unclassified so far: defer to the orchestrator's rebuild loop,
            // which runs the resolver against the optimised IR.  `target_vn`
            // and `addr` are stamped onto the terminator so the lifter can emit
            // a placeholder `Return(target_value)` anchoring the value for that
            // resolver to inspect.  No outgoing edge.  `Withdrawn` holds
            // nothing trustworthy, so it is deferred exactly the same way.
            None | Some(crate::ResolvedTargets::Withdrawn) => {
                self.finish_current_region(RegionTerminator::defer(target_vn, addr))?;
                return Ok(InsnOutcome::RegionClosed);
            }
            Some(crate::ResolvedTargets::LinkRegister) => {
                // Seating a `Return` consumes the site: no placeholder, no
                // `Switch` anchor, so nothing downstream can notice that the
                // seated answer just deleted whatever else the site had.
                // Record it whichever source that answer came from; during the
                // resolve loop this map holds the classifier's derivations too.
                self.builder.link_register_seated.push(addr);
                self.finish_current_region(RegionTerminator::Return)?;
                return Ok(InsnOutcome::RegionClosed);
            }
            Some(&crate::ResolvedTargets::Single(t))
                if self.is_branch_tail_call(PcodeInsnAddr::at_machine_start(t.addr)) =>
            {
                // A `TailCall` consumes the site exactly as `LinkRegister` does
                // (no placeholder, no `Switch` anchor), so a dispatch that
                // really had more arms leaves no trace. Record it, or the loss
                // goes unreported.
                self.builder.tail_call_seated.push(addr);
                self.finish_current_region(RegionTerminator::TailCall { target: t })?;
                return Ok(InsnOutcome::RegionClosed);
            }
            // An in-function single target is a degenerate table.  A one-arm
            // `Switch` keeps the dispatch selector on the terminator, so a
            // later round can re-derive and WIDEN the site once the CFG has
            // grown; a plain branch edge erases the selector and latches the
            // first answer forever.  A switch whose loop back-edge runs through
            // its own arms resolves to one arm that way: before any arm exists
            // the header has a single predecessor, so the index is the entry
            // constant and the table load folds to one literal.  A tail call
            // leaves the function, so it closes no loop through the dispatch.
            Some(seated) => seated.arms(),
        };
        // An out-of-range arm has no per-target tail-call escape; one interior
        // to a region but off every instruction boundary can neither be split
        // out nor found by `switch_arm_regions` at lift time.  Deferring beats
        // failing the whole function over one over-approximated table entry: a
        // bad arm is evidence the whole bound is wrong, and seating the rest
        // decodes whatever else it over-approximated into, which fails the
        // function outright where deferring leaves it lifting with the site
        // unresolved.
        //
        // The off-boundary arm is reported the way
        // `Builder::seat_non_boundary_target` reports the one discovered after
        // seating, so the loss is reported either way.
        let mut deferred = false;
        for target in arms {
            let a = PcodeInsnAddr::at_machine_start(target.addr);
            if self.builder.addr_is_interior_non_boundary(a) {
                self.builder
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
        // Each target decodes in the ISA mode the branch committed
        // (`isa_bit`): an interworking `bx`/`jr`-dispatch table can carry
        // per-target Thumb/ARM (or MIPS16) modes, while a plain jump table
        // commits none and inherits the mode flowing into the branch.
        //
        // Enqueued HIGHEST first so the LIFO work queue explores lowest
        // first. Arms of one switch have no regions yet when the site is
        // sealed, so the interior-address guard above cannot separate
        // them; decoding the lower arm first gives `explore` a region to
        // recognise a higher over-read arm as interior to, which drops it
        // instead of decoding a second region inside that instruction.
        // Read at the branch site, which is still the last address
        // decoded, exactly where `sample_flowing_isa_bit` sampled.
        let flowing = self.flowing_commits()?;
        for target in arms.iter().rev() {
            self.builder.enqueue_resolved(
                region,
                PcodeInsnAddr::at_machine_start(target.addr),
                target.isa_bit.or(flowing_isa),
                addr,
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

    /// Falling through into an already-explored region seals the current one
    /// as `Unconditional` and edges to it.
    ///
    /// A stretch of machine instructions lifting to zero pcode ops (AArch64
    /// `nop` / `paciasp` / `bti`, x86 `nop` / `pause`, alignment padding)
    /// leaves `self.insns` empty when the fall-through fires, and the empty
    /// region owning `self.start_addr` is still materialised: that address can
    /// itself be a branch or switch TARGET, and a target resolves to the region
    /// that *owns* it.
    fn process_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        if let Some(existing) = self.builder.index.get(addr) {
            // A pcode index inside the current machine instruction already
            // starting a region is exactly where two ISA decodes of one address
            // produce different pcode sequences.
            self.seal_onto(addr, existing)?;
            return Ok(InsnOutcome::RegionClosed);
        }
        self.process_new_insn(insn, addr, lift_res)
    }

    /// Decodes machine instructions one at a time until the region closes.
    ///
    /// Decoding MUST stay sequential within a region: `Sleigh::lift_one`
    /// takes `&mut self` and carries context-register state (ARM/Thumb mode
    /// and IT-block state, MIPS16 mode, ppc `linkreg`) that a decoded instruction
    /// can itself commit at a later address, so lifting out of order yields
    /// wrong instructions.
    ///
    /// A region can start mid-machine-instruction when a relative
    /// `CondBranch` jumps into the middle of a pcode sequence, so
    /// `cur_addr.insn_index` may be > 0 on the first iteration.  Calling
    /// `.enumerate()` BEFORE `.skip()` keeps `i` an absolute pcode index and
    /// avoids offset arithmetic.
    pub(super) fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            // So a CONST-space branch target, on this machine address, is
            // never a tail call.
            debug_assert!(
                !self.is_branch_tail_call(cur_addr),
                "decoding {cur_addr:?} outside the function bound"
            );
            let lift_res = match self.lift_code(cur_addr.machine_addr.addr) {
                Ok(lift_res) => lift_res,
                Err(e) if cur_addr.machine_addr != self.start_addr.machine_addr => {
                    // Off a seeded arm, falling into data or unmapped bytes
                    // drops the arm, as running past the end does, unless a
                    // call before them seals it.
                    let off_arm = self.builder.descent.is_some();
                    let here =
                        decode_failure(&e).filter(|f| f.addr() == cur_addr.machine_addr.addr);
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
                    if marked_data && !off_arm {
                        return self.seal_and_report(cur_addr, reason);
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            };
            // `skip` needs a usize.  `insn_index` is either 0 or a value
            // `decode_branch_target` already bounded by `lift_res.insns.len()`,
            // itself a usize, so this cannot truncate.
            #[allow(clippy::cast_possible_truncation)]
            let start_pcode_idx = cur_addr.insn_index as usize;
            // A zero-pcode-op machine instruction (x86 `nop`/`pause`/`endbr64`,
            // AArch64 `nop`/`paciasp`/`bti`, ...) contributes no pcode from here.
            let is_zero_op = lift_res.insns.len() <= start_pcode_idx;

            // Machine-instruction-boundary handling (skipped on the region's own
            // first instruction, where `insn_index` may be mid-pcode after a
            // CondBranch-into-pcode).
            if cur_addr.machine_addr != self.start_addr.machine_addr {
                // Only a zero-pcode-op instruction at `start_addr` can be
                // behind us: both seals below produce an empty region, whose
                // span is those bytes.
                self.note_empty_span_to(cur_addr);
                // Falling through, real or nop alike, into an address that
                // already starts a region: seal here and edge to it rather than
                // decoding those bytes a second time.
                //
                // An EXACT-key lookup, so it only fires on a start this decode
                // lands on.  A start interior to one of the instructions decoded
                // here is stepped over, and those bytes become a second region
                // overlapping the first; region ownership is then not a
                // partition, which `RegionIndex::containing` resolves by
                // walking down past the greatest start.
                if let Some(existing) = self.builder.index.get(cur_addr) {
                    return self.seal_onto(cur_addr, existing);
                }
                // Total segmentation, one region per zero-pcode-op instruction:
                // seal on reaching a nop, and on leaving a lone-nop region.
                // Every nop is then its OWN empty region start and every
                // non-empty region a hole-free run of real instructions with
                // `start_addr == insns[0].addr`, so a branch or switch target
                // either starts a non-empty region or is an empty nop region's
                // start.  The empty regions collapse in the IR (RegionCollapse +
                // PhiCollapse), so this costs only build time.
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
            cur_addr = next_pcode_addr(cur_addr, &lift_res)?;
            if self.seal_past_function_end(cur_addr)? {
                return Ok(());
            }
        }
    }

    /// Records every existing region start STRICTLY inside the machine
    /// instruction at `cur_addr`, which this decode is about to claim for its
    /// own region.
    ///
    /// [`Self::build`]'s fall-through seal is an exact-key lookup, so it only
    /// fires on a start the decode lands on; a start off every boundary of an
    /// instruction decoded here is stepped over, and those bytes end up owned
    /// by two regions with two different instruction streams. That is the same
    /// loss [`Builder::seat_non_boundary_target`] reports, reached from the
    /// other side, so it is reported under the same reason.
    ///
    /// A [`Builder::edge_to_stub`] stub owns no bytes (nothing outside the
    /// function bound is decoded), so its start is no overlap.
    fn note_stepped_over_region_starts(
        &mut self,
        cur_addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) {
        let base = cur_addr.machine_addr.addr;
        let len = lift_res.machine_insn_len as u64;
        let (Some(lo), Some(hi)) = (base.checked_add(1), base.checked_add(len)) else {
            return;
        };
        if lo >= hi {
            return;
        }
        let range = PcodeInsnAddr::at_machine_start(lo)..PcodeInsnAddr::at_machine_start(hi);
        for (start, region) in self.builder.index.starts_in(range) {
            let r = &self.builder.region_graph[region];
            if !(r.insns.is_empty() && matches!(r.terminator, RegionTerminator::TailCall { .. })) {
                self.builder
                    .incomplete
                    .insert(start, crate::Reason::InteriorTarget);
            }
        }
    }

    /// Seals the region onto a [`Builder::edge_to_stub`] stub at `cur_addr`,
    /// where the fall-through found nothing to decode, and reports that
    /// address as `reason`: every stub seal names the address it could not go
    /// on from.
    fn seal_and_report(&mut self, cur_addr: PcodeInsnAddr, reason: crate::Reason) -> Result<()> {
        self.note_empty_span_to(cur_addr);
        let region = self.finish_current_region(RegionTerminator::Unconditional)?;
        self.builder.edge_to_stub(region, cur_addr)?;
        self.builder.incomplete.insert(cur_addr, reason);
        Ok(())
    }

    /// Records the bytes behind `cur_addr` as this region's span, for a seal
    /// that will produce an EMPTY region: only a zero-pcode-op instruction at
    /// `start_addr` can be behind it, and the region then carries no insn to
    /// derive its extent from. A no-op once anything has been decoded.
    fn note_empty_span_to(&mut self, cur_addr: PcodeInsnAddr) {
        if self.insns.is_empty() {
            self.empty_span_len = u32::try_from(
                cur_addr
                    .machine_addr
                    .addr
                    .saturating_sub(self.start_addr.machine_addr.addr),
            )
            .unwrap_or(0);
        }
    }

    /// Ends the region at its last `Call` / `CallIndirect` as `NoReturn` when
    /// the fall-through reached `cur_addr`, which holds no instruction; the
    /// instructions after the call (a PowerPC TOC restore) are dropped with it.
    /// Returns whether the region held a call; `cur_addr` is reported as
    /// `reason`.
    ///
    /// Code after a call that does not decode is what a no-return callee the
    /// build was not told about leaves behind: a literal pool, a traceback
    /// table, a deliberately invalid word.
    fn seal_at_call_not_returning(
        &mut self,
        cur_addr: PcodeInsnAddr,
        reason: crate::Reason,
    ) -> Result<bool> {
        let Some(call) = self.insns.iter().rposition(is_call) else {
            return self.seal_at_preceding_call_not_returning(cur_addr, reason);
        };
        self.insns.truncate(call + 1);
        self.finish_current_region(RegionTerminator::NoReturn)?;
        self.builder.incomplete.insert(cur_addr, reason);
        Ok(true)
    }

    /// [`Self::seal_at_call_not_returning`] for a region holding only
    /// zero-pcode-op instructions, which carries no call of its own: `build`
    /// segments at every one of them, so `call; nop; <not code>` reaches the
    /// seal from the nop's region.
    ///
    /// Walks back over the run (each nop an empty `Unconditional` region with
    /// one predecessor) to the region whose LAST instruction is the call,
    /// re-seals it as `NoReturn` and drops its fall-through edge.  Last, not
    /// anywhere: an instruction after the call means the region was sealed
    /// somewhere other than the nop, so the fall-through is not the call's
    /// return.
    ///
    /// The nops keep their own regions, sealed onto a stub, so a branch
    /// landing on one still has a successor; orphaned otherwise, they go with
    /// `Builder::remove_unreachable_regions`.
    fn seal_at_preceding_call_not_returning(
        &mut self,
        cur_addr: PcodeInsnAddr,
        reason: crate::Reason,
    ) -> Result<bool> {
        if !self.insns.is_empty() {
            return Ok(false);
        }
        let mut current = self.parent_edge;
        while let Some(region_id) = current {
            let region = &self.builder.region_graph[region_id];
            if region.terminator != RegionTerminator::Unconditional {
                return Ok(false);
            }
            match region.insns.last() {
                Some(last) if is_call(last) => {
                    self.seal_and_report(cur_addr, reason)?;
                    let outgoing: Vec<_> = self
                        .builder
                        .region_graph
                        .edges_directed(region_id, petgraph::Outgoing)
                        .map(|e| e.id())
                        .collect();
                    for edge in outgoing {
                        self.builder.region_graph.remove_edge(edge);
                        self.builder.non_boundary_seats.remove(&edge);
                    }
                    self.builder.region_graph[region_id].terminator = RegionTerminator::NoReturn;
                    return Ok(true);
                }
                Some(_) => return Ok(false),
                // Another zero-pcode-op instruction: keep walking while its
                // single predecessor makes the fall-through chain unambiguous.
                None => {
                    let mut preds = self
                        .builder
                        .region_graph
                        .neighbors_directed(region_id, petgraph::Incoming);
                    let pred = preds.next();
                    current = preds.next().is_none().then_some(pred).flatten();
                }
            }
        }
        Ok(false)
    }

    /// Sequential decoding running off the recorded function extent: the
    /// region leaves through a [`Builder::edge_to_stub`] stub at `cur_addr`, the
    /// tail call a branch out of range gets, reported as
    /// [`crate::Reason::PastFunctionEnd`]. Returns whether it sealed.
    ///
    /// Not gated on `insns.is_empty()`: a run of zero-pcode-op instructions
    /// (x86 `nop`, AArch64 `paciasp` / `autiasp`, AArch64 `bti`) advances
    /// `cur_addr` without ever appending to `self.insns`, so such a gate would
    /// let that prefix walk past the bound and absorb the next function's
    /// first real instruction.
    ///
    /// Only a [`crate::FunctionBound::Size`] end gets here. An open bound puts
    /// only addresses below the entry out of range, no region it decodes
    /// starts there, and a fall-through only moves up. Off a seeded arm it is
    /// a [`DecodeFailure::PastEnd`] `Err`, which drops the arm: a real arm stays inside the
    /// function.
    fn seal_past_function_end(&mut self, cur_addr: PcodeInsnAddr) -> Result<bool> {
        if !self.is_branch_tail_call(cur_addr) {
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
            b.incomplete
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

    fn const_vn(offset: u64) -> Vn {
        Vn {
            addr_off: offset,
            addr_space: VnSpace::CONST,
            size: 8,
        }
    }

    fn code_space_vn(space: VnSpace, offset: u64) -> Vn {
        Vn {
            addr_off: offset,
            addr_space: space,
            size: 8,
        }
    }

    /// A CONST branch offset is sign-extended from the varnode's declared
    /// width: a 32-bit-encoded -4 read as 4_294_967_292 fails the whole
    /// function's lift.
    #[test]
    fn const_space_offset_is_sign_extended_from_its_declared_width() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x2000, 0));
        let lift = fake_lift_res(8);
        // -4 at every declared width, a non-power-of-two one included.
        for (size, addr_off) in [
            (1, 0xFC),
            (3, 0xFF_FFFC),
            (4, 0xFFFF_FFFC),
            (8, 0xFFFF_FFFF_FFFF_FFFC),
        ] {
            let narrow = Vn {
                addr_off,
                addr_space: VnSpace::CONST,
                size,
            };
            let target = rb
                .decode_branch_target(narrow, addr_at(0x2000, 6), &lift)
                .expect("a backward CONST branch must decode");
            assert_eq!(target, addr_at(0x2000, 2), "size {size}");
        }
    }

    #[test]
    fn const_space_is_relative_to_current_pcode_insn_index() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x2000, 0));
        let lift = fake_lift_res(8);
        let target = rb
            .decode_branch_target(const_vn(3), addr_at(0x2000, 2), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0x2000, 5));
    }

    #[test]
    fn const_space_with_zero_offset_stays_at_same_pcode_index() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x2000, 0));
        let lift = fake_lift_res(4);
        let target = rb
            .decode_branch_target(const_vn(0), addr_at(0x2000, 2), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0x2000, 2));
    }

    #[test]
    fn default_code_space_is_absolute_machine_address() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let default_cs = b.engine.sleigh.default_code_space();
        let vn = code_space_vn(default_cs, 0xabc0);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let lift = fake_lift_res(1);
        let target = rb
            .decode_branch_target(vn, addr_at(0x1000, 4), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0xabc0, 0));
    }

    #[test]
    fn register_space_returns_invalid_branch_target_error() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let vn = Vn {
            addr_off: 0x10,
            addr_space: VnSpace::REGISTER,
            size: 8,
        };
        let lift = fake_lift_res(1);
        let err = rb
            .decode_branch_target(vn, addr_at(0x1000, 0), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid branch target variable"),
            "got: {err}"
        );
    }

    #[test]
    fn unknown_space_returns_invalid_branch_target_error() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let vn = Vn {
            addr_off: 0x2000,
            addr_space: VnSpace::new(b'x'),
            size: 8,
        };
        let lift = fake_lift_res(1);
        let err = rb
            .decode_branch_target(vn, addr_at(0x1000, 0), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid branch target variable"),
            "got: {err}"
        );
    }

    #[test]
    fn unique_space_returns_invalid_branch_target_error() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let vn = Vn {
            addr_off: 0x40,
            addr_space: VnSpace::UNIQUE,
            size: 8,
        };
        let lift = fake_lift_res(1);
        let err = rb
            .decode_branch_target(vn, addr_at(0x1000, 0), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid branch target variable"),
            "got: {err}"
        );
    }

    #[test]
    fn decode_branch_target_const_space_negative_offset_does_not_wrap() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let vn = Vn {
            addr_off: (-2_i64) as u64,
            addr_space: VnSpace::CONST,
            size: 8,
        };
        let lift = fake_lift_res(8);
        let got = rb
            .decode_branch_target(vn, addr_at(0x1000, 5), &lift)
            .unwrap();
        assert_eq!(got, addr_at(0x1000, 3));
    }

    #[test]
    fn decode_branch_target_const_space_underflow_errors() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let vn = Vn {
            addr_off: (-5_i64) as u64,
            addr_space: VnSpace::CONST,
            size: 8,
        };
        let lift = fake_lift_res(8);
        let err = rb
            .decode_branch_target(vn, addr_at(0x1000, 2), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid branch target variable"),
            "got: {err}"
        );
    }

    #[test]
    fn decode_branch_target_const_space_index_past_end_errors() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let pcode_count = 4u64;
        let lift = fake_lift_res(usize::try_from(pcode_count).unwrap());
        let vn = Vn {
            addr_off: pcode_count + 1,
            addr_space: VnSpace::CONST,
            size: 8,
        };
        let err = rb
            .decode_branch_target(vn, addr_at(0x1000, 0), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid branch target variable"),
            "got: {err}"
        );
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

    #[test]
    fn nocheck_below_start_default_opts_is_tail_call() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(rb.is_branch_tail_call(addr_at(0x0800, 0)));
    }

    #[test]
    fn nocheck_below_start_with_allow_is_not_tail_call() {
        let opts = CfgOptions {
            bound: crate::FunctionBound::Open {
                code_before_start: true,
            },
            ..CfgOptions::default()
        };
        let mut engine = make_engine();
        let mut b = make_builder_opts(0x1000, &mut engine, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(!rb.is_branch_tail_call(addr_at(0x0800, 0)));
    }

    #[test]
    fn nocheck_below_start_with_size_bound_no_allow_is_tail_call() {
        let opts = CfgOptions {
            bound: crate::FunctionBound::size(0x100),
            ..CfgOptions::default()
        };
        let mut engine = make_engine();
        let mut b = make_builder_opts(0x1000, &mut engine, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(rb.is_branch_tail_call(addr_at(0x0800, 0)));
    }

    #[test]
    fn nocheck_within_function_no_limit_is_not_tail_call() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(!rb.is_branch_tail_call(addr_at(0x1200, 0)));
    }

    #[test]
    fn nocheck_at_size_bound() {
        let opts = CfgOptions {
            bound: crate::FunctionBound::size(0x100),
            ..CfgOptions::default()
        };
        let mut engine = make_engine();
        let mut b = make_builder_opts(0x1000, &mut engine, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(rb.is_branch_tail_call(addr_at(0x1100, 0)));
        assert!(!rb.is_branch_tail_call(addr_at(0x10ff, 0)));
    }

    #[test]
    fn size_bound_plus_start_addr_overflow_treats_inside_range_as_non_tail_call() {
        let start_addr = u64::MAX - 0x100;
        let max_size = 0x1000u64;
        let opts = CfgOptions {
            bound: crate::FunctionBound::size(max_size),
            ..CfgOptions::default()
        };
        let mut engine = make_engine();
        let mut b = make_builder_opts(start_addr, &mut engine, &opts);
        let rb = make_region_builder(&mut b, addr_at(start_addr, 0));
        let target = addr_at(start_addr + 0x10, 0);
        assert!(
            !rb.is_branch_tail_call(target),
            "target inside function range must NOT classify as tail call even when start+max overflows"
        );
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
                assert_eq!(addr.machine_addr.addr, base);
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
    fn process_new_insn_branch_with_empty_inputs_errors() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let lift = fake_lift_res(1);
        let bad_insn = rsleigh::Insn {
            opcode: rsleigh::Opcode::Branch,
            inputs: vec![].into(),
            output: None,
        };
        let err = rb
            .process_new_insn(&bad_insn, addr_at(0x1000, 0), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("no target operand"),
            "expected the \"no target operand\" error; got {err}"
        );
    }

    #[test]
    fn process_new_insn_condbranch_with_empty_inputs_errors() {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let mut rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let lift = fake_lift_res(1);
        let bad_insn = rsleigh::Insn {
            opcode: rsleigh::Opcode::CondBranch,
            inputs: vec![].into(),
            output: None,
        };
        let err = rb
            .process_new_insn(&bad_insn, addr_at(0x1000, 0), &lift)
            .unwrap_err();
        assert!(
            err.to_string().contains("no target operand"),
            "expected the \"no target operand\" error; got {err}"
        );
    }
}
