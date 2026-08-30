use petgraph::graph::NodeIndex;

use super::Builder;
use crate::types::{MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator};
use anyhow::{anyhow, bail};

use crate::Result;

fn branch_target_operand(insn: &rsleigh::Insn, addr: PcodeInsnAddr) -> Result<rsleigh::Vn> {
    insn.inputs
        .first()
        .copied()
        .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))
}

/// Advances one pcode index, rolling over to `insn_index = 0` of the next
/// machine instruction at the end of `lift_res`.
fn next_pcode_addr(addr: PcodeInsnAddr, lift_res: &rsleigh::LiftRes) -> Result<PcodeInsnAddr> {
    // Compare in u64 space: usize to u64 is widening everywhere we support,
    // and avoids a potentially-truncating u64 to usize cast.
    let pcode_count = lift_res.insns.len() as u64;
    if addr.insn_index + 1 < pcode_count {
        return Ok(PcodeInsnAddr {
            machine_addr: addr.machine_addr,
            insn_index: addr.insn_index + 1,
        });
    }
    // `rsleigh::LiftRes` imposes no `machine_insn_len > 0` invariant, and a
    // zero-length one re-lifts the same address forever: `cur_addr` never
    // advances, so the build loop's fall-through-OOB guard never fires.
    if lift_res.machine_insn_len == 0 {
        bail!("sleigh returned zero-length machine instruction at pcode addr {addr:?}");
    }
    let next_machine = addr
        .machine_addr
        .addr
        .checked_add(lift_res.machine_insn_len as u64)
        .ok_or_else(|| anyhow!("machine-address overflow advancing past pcode addr {addr:?}"))?;
    Ok(PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: next_machine },
        insn_index: 0,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum InsnOutcome {
    RegionClosed,
    Continue,
}

/// Builds one [`Region`], decoding pcode instructions one at a time.
pub(super) struct RegionBuilder<'b, 'a: 'b, R: rsleigh::MemReader> {
    /// `'b` is the borrow of the Builder, scoped to one `build()` call; `'a`
    /// is the longer Sleigh borrow the Builder itself holds.
    pub(super) builder: &'b mut Builder<'a, R>,
    pub(super) start_addr: PcodeInsnAddr,
    pub(super) insns: Vec<RegionInstruction>,
    /// Bytes consumed by the zero-pcode-op instruction at `start_addr`, set
    /// once decoding leaves it. Read only when the region seals with no
    /// instruction, where nothing else records its span.
    empty_span_len: u32,
    /// `None` only for the function entry region.
    pub(super) parent_edge: Option<NodeIndex>,
    /// The ISA mode this region decodes in, when the arch has one.
    ///
    /// Pinning it once at the region's first address is not enough: Sleigh's
    /// context paint stops at any address a previous write already split, so a
    /// change point INSIDE this region leaves everything past it decoding in
    /// the other region's mode. Re-imposed per instruction, and only written
    /// when a read-back shows it actually drifted, so a region with no barrier
    /// in it costs reads and no parse-cache flush.
    isa_mode: Option<u32>,
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
        }
    }

    /// Re-imposes this region's ISA mode at `addr` when a change point wrote
    /// over it. The read is free; only a genuine drift pays for the write.
    fn hold_isa_mode(&mut self, addr: u64) -> Result<()> {
        let (Some(want), Some(var)) = (self.isa_mode, self.builder.arch.isa_mode_var()) else {
            return Ok(());
        };
        if self.builder.sleigh.get_context_at(addr, var)? != want {
            self.builder.sleigh.set_context_at(addr, var, want)?;
        }
        Ok(())
    }

    /// Re-lifting an address is cheap: `Sleigh` memoises recently-parsed
    /// instructions.
    fn lift_one(&mut self, addr: u64) -> Result<rsleigh::LiftRes> {
        self.hold_isa_mode(addr)?;
        self.builder
            .sleigh
            .lift_one(addr)
            // Display, not Debug: the error's `Debug` nests its source's, which
            // for a reader error carries a captured backtrace.
            .map_err(|e| anyhow!("sleigh could not lift {addr:#x}: {e}"))
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
        let default_code_space = self.builder.sleigh.default_code_space();

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
                let off: i64 = if bits == 64 {
                    raw.cast_signed()
                } else {
                    // Shift the sign bit up to bit 63, then arithmetic-shift
                    // back down.
                    let shift = 64 - bits;
                    (raw << shift).cast_signed() >> shift
                };
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

    /// Address-bounds reasoning only: does NOT check that the target lands on
    /// a machine-instruction boundary.
    pub(super) fn is_branch_tail_call_nocheck(&self, branch_target_addr: PcodeInsnAddr) -> bool {
        crate::is_addr_tail_call(
            branch_target_addr.machine_addr.addr,
            self.builder.start_addr.addr,
            self.builder.options.fn_max_size,
            self.builder.options.allow_code_before_start_addr,
        )
    }

    /// Tail-call vs intra-function, bailing when an out-of-bounds target does
    /// not land on a machine-instruction boundary.
    ///
    /// That bail is defence, not a live case: [`Self::decode_branch_target`]
    /// yields either a CONST-space target on the currently-decoded machine
    /// address (in range, so never a tail call) or an `at_machine_start`, whose
    /// `insn_index` is 0.
    fn classify_branch_target(&self, branch_target_addr: PcodeInsnAddr) -> Result<bool> {
        let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);
        if is_tail_call && branch_target_addr.insn_index != 0 {
            bail!("invalid tail call at opcode {branch_target_addr:?}");
        }
        Ok(is_tail_call)
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
            // The target is a register, so no per-address CC applies and
            // `process_call`'s lookup would read a register offset as an
            // address. The return address still decides whether any
            // in-function code follows.
            rsleigh::Opcode::CallIndirect => {
                if self.is_branch_tail_call_nocheck(next_pcode_addr(addr, lift_res)?) {
                    self.finish_current_region(RegionTerminator::NoReturn)?;
                    Ok(InsnOutcome::RegionClosed)
                } else {
                    Ok(InsnOutcome::Continue)
                }
            }
            _ => Ok(InsnOutcome::Continue),
        }
    }

    /// A `call` normally falls through to its return address, so it is not a
    /// terminator.  Two cases end the region as `NoReturn`, which the IR
    /// lifter lowers to `Call + Unreachable`:
    ///
    /// 1. The target's per-address CC is flagged `no_return` (`exit`/`abort`),
    ///    which ends the region wherever the return address lands.
    /// 2. The return address is outside `[start, start + fn_max_size)`, so no
    ///    in-function code follows: an unmarked no-return callee (FreeBSD
    ///    `exit1`), or the function ends at the call.
    fn process_call(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        // A direct call's target is its first pcode input, a code/RAM-space
        // address constant.
        let target_no_return = insn.inputs.first().is_some_and(|target_vn| {
            self.builder
                .per_address_ccs
                .get(&target_vn.addr_off)
                .is_some_and(|cc| cc.no_return)
        });
        let return_oob = self.is_branch_tail_call_nocheck(next_pcode_addr(addr, lift_res)?);
        if target_no_return || return_oob {
            self.finish_current_region(RegionTerminator::NoReturn)?;
            Ok(InsnOutcome::RegionClosed)
        } else {
            Ok(InsnOutcome::Continue)
        }
    }

    fn process_branch(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_var = branch_target_operand(insn, addr)?;
        let branch_target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
        let is_tail_call = self.classify_branch_target(branch_target_addr)?;
        let terminator = if is_tail_call {
            // A direct `b <oob>` stays in the current ISA mode; no switch.
            RegionTerminator::TailCall {
                target: branch_target_addr.machine_addr.addr.into(),
            }
        } else {
            RegionTerminator::Unconditional
        };
        let region = self.finish_current_region(terminator)?;
        if !is_tail_call {
            self.builder.enqueue(
                Some(region),
                branch_target_addr,
                self.start_addr.machine_addr.addr,
            );
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// The conditional ALWAYS survives.  An out-of-bounds successor is wired
    /// to a synthetic empty stub (`Builder::tail_call_stub`) rather than
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

        // Classify before lifting: an OOB successor would read past
        // `start + fn_max_size`, and where those bytes are zero-pcode-op
        // insns (NOP padding) the lift loop never appends to `self.insns`,
        // so `build()`'s upper-bound truncation never fires.
        let true_oob = self.classify_branch_target(target_addr)?;
        let false_oob = self.is_branch_tail_call_nocheck(next_insn_addr);

        // Edges are unweighted; `region_if` recovers polarity by matching each
        // successor region against `true_target`.  A stub owns exactly its
        // start address, which IS the OOB target, so that matching covers
        // stubs unchanged.
        let region = self.finish_current_region(RegionTerminator::CondBranch {
            true_target: target_addr,
        })?;
        // An OOB successor is wired straight to its (shared, possibly
        // pre-existing) stub and never enqueued, so nothing outside the
        // function bound is ever decoded.  When both arms hit the same OOB
        // address this adds two parallel edges to one stub, mirroring the
        // in-range degenerate case: `region_if` reads the second edge as the
        // fall-through side.
        for (oob, successor) in [(true_oob, target_addr), (false_oob, next_insn_addr)] {
            if oob {
                let stub = self.builder.tail_call_stub(successor)?;
                self.builder.region_graph.add_edge(region, stub, ());
            } else {
                self.builder
                    .enqueue(Some(region), successor, self.start_addr.machine_addr.addr);
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
        let Some(id_vn) = insn.inputs.first() else {
            return Ok(InsnOutcome::Continue);
        };
        if id_vn.addr_space != rsleigh::VnSpace::CONST {
            return Ok(InsnOutcome::Continue);
        }
        let Ok(id_u32) = u32::try_from(id_vn.addr_off) else {
            return Ok(InsnOutcome::Continue);
        };
        let name = self
            .builder
            .user_op_names
            .get(id_u32 as usize)
            .map(String::as_str);
        let overridden = name.and_then(|n| self.builder.options.call_other_overrides.get(n));
        let terminates = match overridden {
            // A caller override states what this binary's build of the op does,
            // which includes the trap masks the built-in rule below reads.
            Some(lookup) => lookup.is_no_return(),
            None => {
                let preset = self.builder.arch.preset();
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
    /// `Switch`.  A site with no entry, or a `Multiple` failing the guard below,
    /// defers via `UnresolvedIndirectBranch`.
    fn process_branch_indirect(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
    ) -> Result<InsnOutcome> {
        let target_vn = branch_target_operand(insn, addr)?;
        let resolved = self.builder.options.seated(addr).cloned();
        // Unclassified so far: defer to the orchestrator's rebuild loop, which
        // runs the resolver against the optimised IR.  `target_vn` and `addr`
        // are stamped onto the terminator so the lifter can emit a
        // placeholder `Return(target_value)` anchoring the value for that
        // resolver to inspect.  No outgoing edge.
        let Some(resolved) = resolved else {
            self.finish_current_region(RegionTerminator::UnresolvedIndirectBranch {
                target_vn,
                addr,
            })?;
            return Ok(InsnOutcome::RegionClosed);
        };
        // An in-function single target is a degenerate table.  A one-arm
        // `Switch` keeps the dispatch selector on the terminator, so a later
        // round can re-derive and WIDEN the site once the CFG has grown; a plain
        // branch edge erases the selector and latches the first answer forever.
        // A switch whose loop back-edge runs through its own arms resolves to
        // one arm that way: before any arm exists the header has a single
        // predecessor, so the index is the entry constant and the table load
        // folds to one literal.  A tail call leaves the function, so it closes
        // no loop through the dispatch.
        let resolved = match resolved {
            crate::ResolvedTargets::Single(t)
                if !self.is_branch_tail_call_nocheck(PcodeInsnAddr::at_machine_start(t.addr)) =>
            {
                crate::ResolvedTargets::Multiple(vec![t])
            }
            other => other,
        };
        match resolved {
            crate::ResolvedTargets::LinkRegister => {
                // Seating a `Return` consumes the site: no placeholder, no
                // `Switch` anchor, so nothing downstream can notice that the
                // seated answer just deleted whatever else the site had.
                // Record it whichever source that answer came from; during the
                // resolve loop this map holds the classifier's derivations too.
                self.builder.link_register_seated.push(addr);
                self.finish_current_region(RegionTerminator::Return)?;
            }
            crate::ResolvedTargets::Single(target) => {
                // Only a tail call reaches here: an in-function `Single` was
                // rewritten to a one-arm `Switch` above.
                //
                // A `TailCall` consumes the site exactly as `LinkRegister` does
                // (no placeholder, no `Switch` anchor), so a dispatch that
                // really had more arms leaves no trace. Record it, or the loss
                // is silent on every channel.
                self.builder.tail_call_seated.push(addr);
                self.finish_current_region(RegionTerminator::TailCall {
                    target: crate::ResolvedTarget::new(target.addr, target.isa_bit),
                })?;
            }
            crate::ResolvedTargets::Multiple(targets) => {
                // An empty target set carries no dispatch information; an
                // out-of-range one has no per-target tail-call escape; one
                // interior to a region but off every instruction boundary can
                // neither be split out nor found by `switch_arm_regions` at lift
                // time.  Deferring beats failing the whole function over one
                // over-approximated table entry.
                if targets.is_empty()
                    || targets.iter().any(|t| {
                        let a = PcodeInsnAddr::at_machine_start(t.addr);
                        self.is_branch_tail_call_nocheck(a)
                            || self.builder.addr_is_interior_non_boundary(a)
                    })
                {
                    self.finish_current_region(RegionTerminator::UnresolvedIndirectBranch {
                        target_vn,
                        addr,
                    })?;
                    return Ok(InsnOutcome::RegionClosed);
                }
                let region = self.finish_current_region(RegionTerminator::Switch {
                    target_vn,
                    targets: targets.clone(),
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
                // A caller's `known_targets` arrives in whatever order it was
                // built, hence the sort.
                let mut ordered = targets.clone();
                ordered.sort_by_key(|t| std::cmp::Reverse(t.addr));
                for target in &ordered {
                    self.builder.enqueue_resolved(
                        Some(region),
                        PcodeInsnAddr::at_machine_start(target.addr),
                        target.isa_bit,
                        addr.machine_addr.addr,
                    );
                }
            }
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// Records `addr` as reached in two ISA modes when the region already
    /// owning it decoded in another.
    ///
    /// Every seal that wires its own edge needs this: the edge never passes
    /// through [`Builder::explore`], so the clash check there never sees it and
    /// an ARM region running into a Thumb-decoded region would reach the caller
    /// as a clean answer.
    fn note_isa_mode_clash(&mut self, addr: PcodeInsnAddr, existing: NodeIndex) {
        if let (Some(mode), Some(&decoded)) =
            (self.isa_mode, self.builder.region_isa_mode.get(&existing))
            && mode != decoded
        {
            self.builder.isa_mode_conflicts.push(addr);
        }
    }

    fn finish_current_region(&mut self, terminator: RegionTerminator) -> Result<NodeIndex> {
        let region = self.builder.add_region(Region {
            start_addr: self.start_addr,
            empty_span_len: if self.insns.is_empty() {
                self.empty_span_len
            } else {
                0
            },
            insns: std::mem::take(&mut self.insns),
            terminator,
        })?;
        if let Some(mode) = self.isa_mode {
            self.builder.region_isa_mode.insert(region, mode);
        }
        if let Some(parent_id) = self.parent_edge {
            self.builder.region_graph.add_edge(parent_id, region, ());
        }
        Ok(region)
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
        if let Some(&existing_region_id) = self.builder.start_addr_to_region_id.get(&addr) {
            // A pcode index inside the current machine instruction already
            // starting a region is exactly where two ISA decodes of one address
            // produce different pcode sequences.
            self.note_isa_mode_clash(addr, existing_region_id);
            let region = self.finish_current_region(RegionTerminator::Unconditional)?;
            self.builder
                .region_graph
                .add_edge(region, existing_region_id, ());
            return Ok(InsnOutcome::RegionClosed);
        }
        self.process_new_insn(insn, addr, lift_res)
    }

    /// Decodes machine instructions one at a time until the region closes.
    ///
    /// Decoding MUST stay sequential within a region: `Sleigh::lift_one`
    /// takes `&mut self` and carries context-register state (ARM/Thumb mode,
    /// x86 segment selectors, MIPS16 mode) that a decoded instruction can
    /// itself modify, so lifting out of order yields wrong instructions.
    ///
    /// A region can start mid-machine-instruction when a relative
    /// `CondBranch` jumps into the middle of a pcode sequence, so
    /// `cur_addr.insn_index` may be > 0 on the first iteration.  Calling
    /// `.enumerate()` BEFORE `.skip()` keeps `i` an absolute pcode index and
    /// avoids offset arithmetic.
    pub(super) fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            let lift_res = self.lift_one(cur_addr.machine_addr.addr)?;
            // `skip` needs a usize.  Pcode count per machine instruction is
            // bounded by Sleigh's per-insn output (<= 256) and usize >= u32
            // everywhere we support, so this cannot truncate.
            #[allow(clippy::cast_possible_truncation)]
            let start_pcode_idx = cur_addr.insn_index as usize;
            // A zero-pcode-op machine instruction (x86 `nop`/`pause`/`endbr64`,
            // AArch64 `nop`/`paciasp`/`bti`, ...) contributes no pcode from here.
            let is_zero_op = lift_res.insns.len() <= start_pcode_idx;

            // Machine-instruction-boundary handling (skipped on the region's own
            // first instruction, where `insn_index` may be mid-pcode after a
            // CondBranch-into-pcode).
            if cur_addr.machine_addr != self.start_addr.machine_addr {
                if self.insns.is_empty() {
                    // Only the zero-pcode-op instruction at `start_addr` is
                    // behind us: both seals below produce an empty region, whose
                    // span is those bytes.
                    self.empty_span_len = u32::try_from(
                        cur_addr
                            .machine_addr
                            .addr
                            .saturating_sub(self.start_addr.machine_addr.addr),
                    )
                    .unwrap_or(0);
                }
                // Falling through, real or nop alike, into an address that
                // already starts a region: seal here and edge to it rather than
                // decoding those bytes a second time.
                //
                // An EXACT-key lookup, so it only fires on a start this decode
                // lands on.  A start interior to one of the instructions decoded
                // here is stepped over, and those bytes become a second region
                // overlapping the first; region ownership is then not a
                // partition, which `Builder::find_region_containing_addr`
                // resolves by walking down past the greatest start.
                if let Some(&existing) = self.builder.start_addr_to_region_id.get(&cur_addr) {
                    self.note_isa_mode_clash(cur_addr, existing);
                    let region = self.finish_current_region(RegionTerminator::Unconditional)?;
                    self.builder.region_graph.add_edge(region, existing, ());
                    return Ok(());
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
                    self.builder
                        .enqueue(Some(region), cur_addr, self.start_addr.machine_addr.addr);
                    return Ok(());
                }
            }

            for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
                cur_addr.insn_index = i as u64;
                let res = self.process_insn(insn, cur_addr, &lift_res)?;
                if res == InsnOutcome::RegionClosed {
                    return Ok(());
                }
            }
            cur_addr = next_pcode_addr(cur_addr, &lift_res)?;
            self.detect_fallthrough_oob_tail_call(cur_addr)?;
        }
    }

    /// Sequential decoding running off the recorded function extent is a
    /// function-boundary error, NOT a tail call: a real tail call has an
    /// explicit `jmp`/`je` opcode, so reaching the bound by falling through
    /// means `fn_max_size` is too small or the function is unterminated.
    ///
    /// Gated on `cur_addr` having advanced past the region start: a run of
    /// zero-pcode-op instructions (x86 `nop`, AArch64 `paciasp` / `autiasp`,
    /// AArch64 `bti`) advances `cur_addr` without ever appending to `self.insns`,
    /// so a gate on `insns.is_empty()` would let such a prefix walk past the
    /// bound and absorb the next function's first real instruction.
    fn detect_fallthrough_oob_tail_call(&mut self, cur_addr: PcodeInsnAddr) -> Result<()> {
        let advanced_past_start = cur_addr.machine_addr.addr != self.start_addr.machine_addr.addr;
        if !advanced_past_start || !self.is_branch_tail_call_nocheck(cur_addr) {
            return Ok(());
        }
        let start = self.builder.start_addr.addr;
        let fn_max_size = self.builder.options.fn_max_size;
        anyhow::bail!(
            "function-boundary error at {cur_addr:?}: sequential decoding overflowed past \
             [start={start:#x}, start + fn_max_size={fn_max_size:?}); function is unterminated \
             within its recorded extent (likely cause: `fn_max_size` is too small for the \
             function, OR the binary ends mid-function)"
        );
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let existing = b
            .add_region(make_region(&[(0x1000, 3)]))
            .expect("add_region");
        b.region_isa_mode.insert(existing, 1);
        let mut rb = RegionBuilder::new(&mut b, addr_at(0x1000, 0), None, Some(0));
        let lift = fake_lift_res(4);

        let outcome = rb
            .process_insn(&fake_insn(), addr_at(0x1000, 3), &lift)
            .expect("process_insn");

        assert_eq!(outcome, InsnOutcome::RegionClosed);
        assert_eq!(
            b.isa_mode_conflicts,
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
        let mut sleigh =
            rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
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

    #[test]
    fn const_space_is_relative_to_current_pcode_insn_index() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let rb = make_region_builder(&mut b, addr_at(0x2000, 0));
        let lift = fake_lift_res(8);
        let target = rb
            .decode_branch_target(const_vn(3), addr_at(0x2000, 2), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0x2000, 5));
    }

    #[test]
    fn const_space_with_zero_offset_stays_at_same_pcode_index() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let rb = make_region_builder(&mut b, addr_at(0x2000, 0));
        let lift = fake_lift_res(4);
        let target = rb
            .decode_branch_target(const_vn(0), addr_at(0x2000, 2), &lift)
            .unwrap();
        assert_eq!(target, addr_at(0x2000, 2));
    }

    #[test]
    fn default_code_space_is_absolute_machine_address() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let default_cs = b.sleigh.default_code_space();
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(rb.is_branch_tail_call_nocheck(addr_at(0x0800, 0)));
    }

    #[test]
    fn nocheck_below_start_with_allow_is_not_tail_call() {
        let opts = CfgOptions {
            allow_code_before_start_addr: true,
            ..CfgOptions::default()
        };
        let mut sleigh = make_sleigh();
        let mut b = make_builder_opts(0x1000, &mut sleigh, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(!rb.is_branch_tail_call_nocheck(addr_at(0x0800, 0)));
    }

    #[test]
    fn nocheck_below_start_with_allow_and_fn_max_size_is_tail_call() {
        let opts = CfgOptions {
            allow_code_before_start_addr: true,
            fn_max_size: Some(0x100),
            ..CfgOptions::default()
        };
        let mut sleigh = make_sleigh();
        let mut b = make_builder_opts(0x1000, &mut sleigh, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(
            rb.is_branch_tail_call_nocheck(addr_at(0x0800, 0)),
            "with fn_max_size set, backward jumps below start must be tail calls regardless of allow_code_before_start_addr"
        );
    }

    #[test]
    fn nocheck_below_start_with_fn_max_size_no_allow_is_tail_call() {
        let opts = CfgOptions {
            fn_max_size: Some(0x100),
            ..CfgOptions::default()
        };
        let mut sleigh = make_sleigh();
        let mut b = make_builder_opts(0x1000, &mut sleigh, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(rb.is_branch_tail_call_nocheck(addr_at(0x0800, 0)));
    }

    #[test]
    fn nocheck_within_function_no_limit_is_not_tail_call() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(!rb.is_branch_tail_call_nocheck(addr_at(0x1200, 0)));
    }

    #[test]
    fn nocheck_at_fn_max_size_boundary() {
        let opts = CfgOptions {
            fn_max_size: Some(0x100),
            ..CfgOptions::default()
        };
        let mut sleigh = make_sleigh();
        let mut b = make_builder_opts(0x1000, &mut sleigh, &opts);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        assert!(rb.is_branch_tail_call_nocheck(addr_at(0x1100, 0)));
        assert!(!rb.is_branch_tail_call_nocheck(addr_at(0x10ff, 0)));
    }

    #[test]
    fn fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call() {
        let start_addr = u64::MAX - 0x100;
        let max_size = 0x1000u64;
        let opts = CfgOptions {
            fn_max_size: Some(max_size),
            ..CfgOptions::default()
        };
        let mut sleigh = make_sleigh();
        let mut b = make_builder_opts(start_addr, &mut sleigh, &opts);
        let rb = make_region_builder(&mut b, addr_at(start_addr, 0));
        let target = addr_at(start_addr + 0x10, 0);
        assert!(
            !rb.is_branch_tail_call_nocheck(target),
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
        let mut sleigh = make_sleigh_over(bytes, base);
        let mut b = make_builder(base, &mut sleigh);
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
        let mut sleigh = make_sleigh_over(bytes, base);
        let mut b = make_builder(base, &mut sleigh);
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
        let mut sleigh = make_sleigh_over(bytes, base);
        let mut b = make_builder(base, &mut sleigh);
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
        let mut sleigh = make_sleigh_over(bytes, base);
        let mut b = make_builder(base, &mut sleigh);
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
        let mut sleigh = make_sleigh_over(bytes, base);
        let mut b = make_builder(base, &mut sleigh);
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
        let mut sleigh = make_sleigh_over(bytes, base);
        let mut b = make_builder(base, &mut sleigh);
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
    fn finish_current_region_empty_insns_returns_error() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let mut rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let err = rb
            .finish_current_region(RegionTerminator::Return)
            .unwrap_err();
        assert!(
            err.to_string().contains("has no instructions"),
            "got: {err}"
        );
    }

    #[test]
    fn process_new_insn_branch_with_empty_inputs_errors() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
            "expected MissingBranchTarget; got {err}"
        );
    }

    #[test]
    fn process_new_insn_condbranch_with_empty_inputs_errors() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
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
            "expected MissingBranchTarget; got {err}"
        );
    }
}
