use petgraph::graph::NodeIndex;

use super::Builder;
use crate::types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction, RegionTerminator,
};
use anyhow::{anyhow, bail};

use crate::Result;

/// Returns the [`PcodeInsnAddr`] that comes immediately after `addr` within
/// the lifted machine instruction `lift_res`.
///
/// - If `addr.insn_index + 1` is still within `lift_res.insns`, returns the
///   same machine address with `insn_index` advanced by one.
/// - Otherwise returns the start (`insn_index = 0`) of the *next* machine
///   instruction.
///
/// # Errors
/// Returns an error when the current machine address plus
/// `lift_res.machine_insn_len` overflows `u64`.
fn next_pcode_addr(
    addr: PcodeInsnAddr,
    lift_res: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
    // Compare in u64 space: usize → u64 is widening on every supported
    // target and avoids a potentially-truncating u64 → usize cast.
    let pcode_count = lift_res.insns.len() as u64;
    if addr.insn_index + 1 < pcode_count {
        return Ok(PcodeInsnAddr {
            machine_addr: addr.machine_addr,
            insn_index: addr.insn_index + 1,
        });
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

/// Outcome of processing a single pcode instruction inside the region
/// builder.
///
/// Created internally by `Builder::explore`; not part of the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum InsnOutcome {
    /// The instruction terminated the current region (branch, return, or
    /// fall-through into an already-existing region).
    RegionClosed,
    /// The instruction did not terminate the region; decoding continues.
    Continue,
}

/// Builds a single [`Region`] by decoding pcode instructions one at a time.
///
/// Created internally by `Builder::explore`; not part of the public API.
/// Holds a mutable reference back to the parent [`Builder`] so it can
/// enqueue successor regions and call `Builder::add_region`.
pub(super) struct RegionBuilder<'b, 'a: 'b, R: rsleigh::MemReader> {
    /// Parent builder — used to access the Sleigh context, options, graph,
    /// and work queue.  Two lifetimes: `'b` is the borrow of the Builder
    /// itself (short-lived, scoped to one `RegionBuilder::build()` call),
    /// and `'a` is the Sleigh borrow the Builder holds (outlives `'b`).
    pub(super) builder: &'b mut Builder<'a, R>,
    /// Address of the first instruction this region will contain.
    pub(super) start_addr: PcodeInsnAddr,
    /// Instructions accumulated so far.
    pub(super) insns: Vec<RegionInstruction>,
    /// The predecessor region this one will be wired to, if any.
    /// `None` only for the function entry region.  Edges are unweighted;
    /// the predecessor's terminator classifies the transfer.
    pub(super) parent_edge: Option<NodeIndex>,
}

impl<'b, 'a: 'b, R: rsleigh::MemReader> RegionBuilder<'b, 'a, R> {
    pub(super) fn new(
        builder: &'b mut Builder<'a, R>,
        start_addr: PcodeInsnAddr,
        parent_edge: Option<NodeIndex>,
    ) -> Self {
        RegionBuilder {
            builder,
            start_addr,
            insns: Vec::new(),
            parent_edge,
        }
    }

    /// Lift a single machine instruction at `addr`.  Thin wrapper over
    /// `Sleigh::lift_one` that converts the rsleigh error into an
    /// `anyhow::Error` so the rest of the region-builder pipeline can
    /// use `?` uniformly.  GHIDRA's C++ `DisassemblyCache`
    /// (`sleigh.hh:107-120`) already memoises recently-parsed
    /// instructions inside the `Sleigh` instance, so no outer cache is
    /// needed.
    fn lift_one(&mut self, addr: u64) -> Result<rsleigh::LiftRes> {
        self.builder
            .sleigh
            .lift_one(addr)
            .map_err(|e| anyhow!("generic sleigh error {e:?}"))
    }

    /// Decodes a pcode branch-target varnode into a [`PcodeInsnAddr`].
    ///
    /// Pcode encodes branch targets in two ways:
    /// - **Relative** (`VnSpace::CONST`): the target is a pcode-instruction
    ///   index *offset* within the same machine instruction. The resulting
    ///   index must lie within `lift_res.insns` — Sleigh's intra-instruction
    ///   contract guarantees CONST-space branches stay inside the current
    ///   machine instruction's pcode sequence.
    /// - **Absolute** (default code space): the target is a raw virtual
    ///   address; the pcode index is implicitly 0 (start of machine insn).
    ///
    /// `lift_res` is the lifted result for the machine instruction containing
    /// the branch — only its `insns.len()` is read, to bound the CONST-space
    /// target index.
    fn decode_branch_target(
        &self,
        branch_target_var: rsleigh::Vn,
        branch_insn_addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<PcodeInsnAddr> {
        let default_code_space = self.builder.sleigh.default_code_space();

        match branch_target_var.addr_space {
            // CONST-space: pcode-local relative branch. The "address" is a
            // signed offset on the *pcode index* within the same machine
            // instruction, two's-complement-encoded into the u64 `off` (so
            // `(-n) as u64` for backward branches). `cast_signed` is the
            // bit-pattern-preserving u64→i64 reinterpretation; `checked_add_signed`
            // catches either-direction overflow on the resulting index. The
            // bounds check then ensures the target lies in `0..lift_res.insns.len()` —
            // an out-of-range index would otherwise be silently skipped by the
            // build loop, which would advance past the end of the current
            // machine instruction's pcode sequence and produce a wrong CFG with
            // no diagnostic.
            rsleigh::VnSpace::CONST => {
                // Sign-extend the encoded offset from the varnode's declared
                // byte width before treating it as a signed i64.  Without this
                // a 32-bit-encoded -4 (= 0xFFFFFFFC) reads as the giant
                // positive number 4_294_967_292 when cast straight from u64,
                // and the bounds check below incorrectly rejects the target.
                let raw = branch_target_var.addr_off;
                let off: i64 = match branch_target_var.size {
                    1 => i64::from(raw as i8),
                    2 => i64::from(raw as i16),
                    4 => i64::from(raw as i32),
                    8 => raw.cast_signed(),
                    other => bail!(
                        "unsupported branch-target varnode size {other} at opcode {branch_insn_addr:?}"
                    ),
                };
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or_else(
                    || anyhow!("invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"),
                )?;
                // `usize → u64` is infallible on every supported target (32/64-bit).
                let pcode_count = lift_res.insns.len() as u64;
                // Sleigh idiom: a branch to `target == pcode_count` (one past the
                // last pcode insn) means "exit the current pcode block, fall
                // through to the next machine instruction". MIPS DIV / SLT
                // emit this for their conditional traps. Compute the next
                // machine-insn address for that case; reject anything strictly
                // beyond.
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
            // Absolute branch: the offset IS the target machine
            // address.  Sleigh emits the target as the full 64-bit
            // `off` regardless of the varnode's declared `size` — the
            // CONST arm above sign-extends because it carries a
            // signed pcode-index *offset*, but absolute targets are
            // unsigned virtual addresses with no size-dependent
            // sign-extension.  `size` is therefore intentionally
            // ignored here.
            space if space == default_code_space => {
                Ok(PcodeInsnAddr::at_machine_start(branch_target_var.addr_off))
            }
            _ => Err(anyhow!(
                "invalid branch target variable {branch_target_var:?} at opcode {branch_insn_addr:?}"
            )),
        }
    }

    /// Checks whether `branch_target_addr` should be treated as a tail call
    /// using only address-bounds reasoning (no `insn_index` validation).
    ///
    /// Delegates to [`crate::is_addr_tail_call`] for the predicate; this
    /// method is the cfg-builder convenience wrapper that pulls
    /// `start_addr` / `fn_max_size` / `allow_code_before_start_addr` from
    /// the builder's options.
    ///
    /// Callers that need to enforce the well-formedness rule "a tail call
    /// may only target the first pcode instruction of a machine
    /// instruction" should inline that `insn_index == 0` validation
    /// themselves at the use site (see the `Branch` and `CondBranch` arms
    /// of [`Self::process_new_insn`]).
    pub(super) fn is_branch_tail_call_nocheck(&self, branch_target_addr: PcodeInsnAddr) -> bool {
        crate::is_addr_tail_call(
            branch_target_addr.machine_addr.addr,
            self.builder.start_addr.addr,
            self.builder.options.fn_max_size,
            self.builder.options.allow_code_before_start_addr,
        )
    }

    /// Processes `insn` as a fresh instruction (not already in any region).
    ///
    /// Appends the instruction to the current region, then dispatches on the
    /// opcode to a per-opcode helper.  Anything not listed below returns
    /// [`InsnOutcome::Continue`] so the outer decode loop
    /// keeps lifting.
    fn process_new_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        self.insns.push(RegionInstruction {
            addr,
            insn: insn.clone(),
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
            _ => Ok(InsnOutcome::Continue),
        }
    }

    /// Handles a `Branch` opcode: decode the target, classify as tail-call
    /// vs intra-function branch, finalise the region, and enqueue the
    /// successor (via a plain unweighted `()` edge) when it's not a tail call.
    fn process_branch(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_var = *insn
            .inputs
            .first()
            .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))?;
        let branch_target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
        let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);
        if is_tail_call && branch_target_addr.insn_index != 0 {
            bail!("invalid tail call at opcode {branch_target_addr:?}");
        }
        let terminator = if is_tail_call {
            RegionTerminator::TailCall {
                target: branch_target_addr.machine_addr.addr,
            }
        } else {
            RegionTerminator::Unconditional
        };
        let region = self.finish_current_region(terminator)?;
        if !is_tail_call {
            // Not a tail call — enqueue the target so the builder explores it
            // next.  Edges are unweighted; the `Unconditional` terminator
            // records that this region ended with a branch opcode, and the
            // IR lifter wires the unconditional successor through the
            // region linker.
            self.builder.work_queue.push((Some(region), branch_target_addr));
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// Handles a `CondBranch` opcode: decode the taken/not-taken successors,
    /// pre-classify each against the function bounds, then finalise the
    /// region with the matching terminator.  See the per-arm comments below
    /// for the four cases (both in-range / both OOB / one-OOB).
    fn process_cond_branch(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        let target_var = *insn
            .inputs
            .first()
            .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))?;
        let target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
        let next_insn_addr = next_pcode_addr(addr, lift_res)?;

        // Pre-classify both successors against the function bounds.
        // Lifting an OOB successor address would otherwise read past
        // `start + fn_max_size`, and on architectures where the OOB
        // bytes happen to be zero-pcode-op insns (e.g. NOP padding)
        // the inner lift loop never appends to `self.insns`, so the
        // upper-bound truncation in `build()` never fires.
        let true_oob = self.is_branch_tail_call_nocheck(target_addr);
        if true_oob && target_addr.insn_index != 0 {
            bail!("invalid tail call at opcode {target_addr:?}");
        }
        let false_oob = self.is_branch_tail_call_nocheck(next_insn_addr);

        match (true_oob, false_oob) {
            (false, false) => {
                // Both in-range — original CondBranch behaviour.  Record the
                // taken successor's address on the terminator; both outgoing
                // edges are unweighted, and `region_if` recovers the polarity
                // by matching each successor's start_addr against `true_target`.
                let region = self.finish_current_region(RegionTerminator::CondBranch {
                    true_target: target_addr,
                })?;
                self.builder.work_queue.push((Some(region), target_addr));
                self.builder.work_queue.push((Some(region), next_insn_addr));
            }
            (true, true) => {
                // Both successors leave the function — collapse to a
                // single TailCall to the taken target.  The IR layer
                // lifts this as `Call(IntConst(target)) + Return`,
                // and `SpecialTerm::TailCall::skips_opcode` is
                // extended to also skip the trailing `CondBranch`
                // insn that lives in `self.insns`.
                self.finish_current_region(RegionTerminator::TailCall {
                    target: target_addr.machine_addr.addr,
                })?;
            }
            (true, false) | (false, true) => {
                // Exactly one successor leaves the function.  Pop
                // the trailing `CondBranch` insn from `self.insns`
                // so the IR's per-region loop does not re-route it
                // through `handle_cond_branch` (which would fail
                // looking up the missing OOB edge), and emit
                // `RegionTerminator::Unconditional` to the in-range
                // successor.  The conditional is lost, but the
                // lift completes.  The in-range successor is
                // preserved as a regular intra-function branch
                // via `add_region`'s relaxed empty-Unconditional
                // invariant — `add_region` accepts empty regions
                // terminated with Unconditional (the degenerate
                // single-instruction case is sound by the same
                // path).
                let in_range = if true_oob { next_insn_addr } else { target_addr };
                // Pop the trailing CondBranch from `self.insns`
                // so the IR's per-region loop does not re-route
                // it through `handle_cond_branch` (which would
                // fail looking up the missing OOB edge).  Even
                // when this leaves the region empty
                // (single-instruction case), `add_region` now
                // accepts empty regions terminated with
                // Unconditional.  The IR-layer per-region driver
                // iterates `region.insns` (a no-op for empty
                // insns) and handles the Unconditional terminator
                // + outgoing edge separately, so the in-range
                // successor is preserved as a regular
                // intra-function branch.
                self.insns.pop();
                let region = self.finish_current_region(RegionTerminator::Unconditional)?;
                self.builder.work_queue.push((Some(region), in_range));
            }
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// Handles a `CallOther` opcode: resolve the user-op id from the
    /// CONST input at position 0, classify via the target ABI table, and
    /// finalise the region with `NoReturn` for the noreturn family.
    /// Unexpected input shapes and all other classifications fall through
    /// to today's behaviour ([`InsnOutcome::Continue`]) —
    /// the IR layer's strict-on-emission check will surface any real
    /// problem with full context.
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
        let name = self.builder.sleigh.user_op_name(id_u32);
        let preset = self.builder.arch.preset();
        let class = name.and_then(|n| strider_target::call_other_abi::classify(preset, n));
        if matches!(class, Some(strider_target::call_other_abi::CallOtherClass::NoReturn)) {
            // CallOther is already in self.insns from the
            // process_new_insn prologue push; finish_current_region
            // carries it.  Trailing BranchIndirect is never decoded.
            self.finish_current_region(RegionTerminator::NoReturn)?;
            return Ok(InsnOutcome::RegionClosed);
        }
        Ok(InsnOutcome::Continue)
    }

    /// Handles a `BranchIndirect` opcode by looking up a cached
    /// `known_targets` entry (seeded by the orchestrator's
    /// rebuild-driven loop from the IR-level indirect-branch resolver)
    /// and finalising the region with the matching terminator:
    /// - `Single(K)` inside the function range → `Unconditional` to K
    ///   (enqueue successor for exploration).
    /// - `Single(K)` outside the function range → `TailCall { target:
    ///   K }` (no successor edge).
    /// - `LinkRegister` → `Return` (no successor edge).
    /// - `Multiple` → `Switch` (one `Unconditional` edge per target).  If
    ///   any target is OOB, defer the whole site via
    ///   `UnresolvedIndirectBranch` — Switch has no per-target
    ///   tail-call escape, and encoding mixed in-range / tail-call
    ///   targets in a single Switch would misroute the OOB cases.
    /// - unresolvable → defer via `UnresolvedIndirectBranch` for the
    ///   strider-level outer loop.
    ///
    /// `CallIndirect` is intentionally NOT routed here — it remains a
    /// non-terminator opcode handled by the IR layer.
    fn process_branch_indirect(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
    ) -> Result<InsnOutcome> {
        let target_vn = *insn
            .inputs
            .first()
            .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))?;
        // Only a pre-classified `known_targets` entry (seeded by the
        // orchestrator's rebuild-driven loop from the IR-level resolver)
        // seats a terminator here.  Every other `BranchIndirect` is
        // deferred via `UnresolvedIndirectBranch` for the orchestrator to
        // classify against the optimised IR on the next rebuild.
        let resolved = self.builder.options.known_targets.get(&addr).cloned();
        // None means this site has not been classified yet — defer to
        // the orchestrator's rebuild loop, which runs the IR-level
        // indirect-branch resolver on the optimised IR.
        // Stamp `target_vn` and `addr` onto the deferred terminator so
        // the strider lifter can emit a placeholder
        // `Return(target_value)` anchoring the value for IR-level
        // indirect-branch resolver inspection.  No outgoing edge.
        let Some(resolved) = resolved else {
            self.finish_current_region(
                RegionTerminator::UnresolvedIndirectBranch { target_vn, addr },
            )?;
            return Ok(InsnOutcome::RegionClosed);
        };
        match resolved {
            super::ResolvedTargets::LinkRegister => {
                self.finish_current_region(RegionTerminator::Return)?;
            }
            super::ResolvedTargets::Single(target) => {
                let target_addr = PcodeInsnAddr::at_machine_start(target);
                // `_nocheck` is sufficient: `at_machine_start` pins
                // `insn_index == 0`, so the validating variant has
                // nothing to validate.
                self.finish_branch_or_tail_call(
                    target_addr,
                    self.is_branch_tail_call_nocheck(target_addr),
                )?;
            }
            super::ResolvedTargets::Multiple(targets) => {
                // `Multiple` is a jump-table classification produced by
                // the IR-level resolver and fed back via `known_targets`.
                //
                // Defend the documented non-empty invariant: an empty target
                // set carries no dispatch information, so treat it as
                // unresolved rather than emit a Switch region with zero edges.
                if targets.is_empty() {
                    self.finish_current_region(
                        RegionTerminator::UnresolvedIndirectBranch { target_vn, addr },
                    )?;
                    return Ok(InsnOutcome::RegionClosed);
                }
                let any_out_of_range = targets.iter().any(|t| {
                    self.is_branch_tail_call_nocheck(PcodeInsnAddr::at_machine_start(*t))
                });
                if any_out_of_range {
                    self.finish_current_region(
                        RegionTerminator::UnresolvedIndirectBranch { target_vn, addr },
                    )?;
                    return Ok(InsnOutcome::RegionClosed);
                }
                let region = self.finish_current_region(RegionTerminator::Switch {
                    target_vn,
                    targets: targets.clone(),
                })?;
                for target in targets {
                    let target_addr = PcodeInsnAddr::at_machine_start(target);
                    self.builder.work_queue.push((Some(region), target_addr));
                }
            }
        }
        Ok(InsnOutcome::RegionClosed)
    }

    /// Finalises the region that has been accumulating instructions.
    ///
    /// Calls `Builder::add_region` (which rejects empty regions) and, if
    /// there is a parent edge, adds that edge to the graph. Returns the
    /// new region's [`NodeIndex`].
    fn finish_current_region(&mut self, terminator: RegionTerminator) -> Result<NodeIndex> {
        let region = self.builder.add_region(Region {
            start_addr: self.start_addr,
            insns: std::mem::take(&mut self.insns),
            terminator,
        })?;
        if let Some(parent_id) = self.parent_edge {
            self.builder.region_graph.add_edge(parent_id, region, ());
        }
        Ok(region)
    }

    /// Either finishes the current region with `RegionTerminator::TailCall`
    /// (when `is_tail_call`) or with `RegionTerminator::Unconditional` plus an
    /// outgoing edge to `target_addr` enqueued for further exploration.
    /// Shared between the `Branch` opcode arm and
    /// `process_branch_indirect`'s `Single` path — both classify a single
    /// jump target the same way (intra-function vs OOB).
    fn finish_branch_or_tail_call(
        &mut self,
        target_addr: PcodeInsnAddr,
        is_tail_call: bool,
    ) -> Result<()> {
        if is_tail_call {
            self.finish_current_region(RegionTerminator::TailCall {
                target: target_addr.machine_addr.addr,
            })?;
        } else {
            let region = self.finish_current_region(RegionTerminator::Unconditional)?;
            self.builder.work_queue.push((Some(region), target_addr));
        }
        Ok(())
    }

    /// Processes `insn` at `addr`, first checking whether `addr` is already
    /// the start of a known region.
    ///
    /// If so, the current region has fallen through into an already-explored
    /// region: the current region is finalised with an `Unconditional` terminator
    /// and an (unweighted) edge is added to the existing region.
    /// Otherwise delegates to [`process_new_insn`](Self::process_new_insn).
    ///
    /// **Zero-pcode-op stretch case.**  When the outer `build` loop walks
    /// across one or more machine instructions that lift to zero pcode
    /// ops (AArch64 `nop` / `paciasp` / `autiasp`, ARM `bti`, etc.),
    /// `self.insns` is still empty by the time fall-through into an
    /// already-explored region fires.  Creating an empty intermediate
    /// region would violate `add_region`'s non-empty invariant.
    /// Instead, the parent edge is hot-wired straight into the existing
    /// region with the parent's original edge kind preserved.  The
    /// effect on the resulting CFG is the same as if the empty stretch
    /// were a one-region pass-through: the parent's classification
    /// flows directly to the explored successor.
    fn process_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<InsnOutcome> {
        // If `addr` is the start of an already-explored region, the current region
        // fell through to it: finalise the current region and add an Unconditional edge.
        if let Some(&existing_region_id) = self.builder.start_addr_to_region_id.get(&addr) {
            if self.insns.is_empty() {
                if let Some(parent_id) = self.parent_edge {
                    self.builder
                        .region_graph
                        .add_edge(parent_id, existing_region_id, ());
                }
                return Ok(InsnOutcome::RegionClosed);
            }
            let region = self.finish_current_region(RegionTerminator::Unconditional)?;
            self.builder
                .region_graph
                .add_edge(region, existing_region_id, ());
            return Ok(InsnOutcome::RegionClosed);
        }
        self.process_new_insn(insn, addr, lift_res)
    }

    /// Main decode loop: lifts machine instructions one at a time and calls
    /// [`process_insn`](Self::process_insn) for each pcode instruction until
    /// the region is complete.
    ///
    /// # Pcode index accounting
    ///
    /// When a region starts at a non-zero pcode index (because a relative
    /// `CondBranch` branched into the middle of a machine instruction's pcode
    /// sequence), `cur_addr.insn_index` may be > 0 at the top of the first
    /// iteration.  By calling `.enumerate()` *before* `.skip(start_pcode_idx)`,
    /// the enumerator's index `i` is already the absolute pcode-instruction
    /// index within the current machine instruction, so no offset arithmetic
    /// is needed.  Subsequent machine instructions always start at pcode
    /// index 0, so `start_pcode_idx` is naturally 0 there.
    pub(super) fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            let lift_res = self.lift_one(cur_addr.machine_addr.addr)?;
            // `enumerate` before `skip` so `i` is the absolute pcode index.
            // On the very first machine instruction this may start at a non-zero
            // index (the work queue delivered a mid-instruction entry point);
            // subsequent machine instructions always start at 0.
            //
            // `Iterator::skip` requires `usize`. Pcode counts per machine
            // instruction are bounded by Sleigh's per-insn output (≤ 256); on
            // every supported target `usize ≥ u32`, so the cast cannot truncate.
            #[allow(clippy::cast_possible_truncation)]
            let start_pcode_idx = cur_addr.insn_index as usize;
            for (i, insn) in lift_res.insns.iter().enumerate().skip(start_pcode_idx) {
                cur_addr.insn_index = i as u64;
                let res = self.process_insn(insn, cur_addr, &lift_res)?;
                if matches!(res, InsnOutcome::RegionClosed) {
                    return Ok(());
                }
            }
            // We're done exploring a single machine insn, continue to the next one
            cur_addr = next_pcode_addr(cur_addr, &lift_res)?;
            self.detect_fallthrough_oob_tail_call(cur_addr)?;
        }
    }

    /// Detects sequential-decode fall-through across `start + fn_max_size`
    /// and surfaces it as a hard error.
    ///
    /// Sequential decoding running off the recorded function extent without
    /// an explicit terminator opcode is a **function-boundary error**, not a
    /// tail call: a legitimate tail call has an explicit `jmp <oob>` /
    /// `je <oob>` opcode and reaches `is_branch_tail_call_nocheck` through
    /// [`Self::process_branch`] / [`Self::process_cond_branch`] — those
    /// classify as [`RegionTerminator::TailCall`] correctly.  Sequential
    /// fall-through means the user's `fn_max_size` is too small or the
    /// function is unterminated within its recorded extent; silently
    /// classifying that as a tail call hides the bug.
    ///
    /// Empty-`insns` guard: if every machine instruction so far decoded to
    /// zero pcode ops (true NOPs on some Sleigh specs), the inner `for` loop
    /// never appended to `self.insns`.  In that degenerate case we cannot
    /// have advanced past anything yet — return `Ok(())` so the outer loop
    /// keeps lifting.
    fn detect_fallthrough_oob_tail_call(&mut self, cur_addr: PcodeInsnAddr) -> Result<()> {
        if self.insns.is_empty() || !self.is_branch_tail_call_nocheck(cur_addr) {
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
    //! Tests for `next_pcode_addr`, `RegionBuilder::decode_branch_target`,
    //! and `RegionBuilder::is_branch_tail_call_nocheck`.
    //!
    //! Ported from pre-rewrite
    //! `crates/cfg/tests/{region_builder_decode,region_builder_tail_call}.rs`.
    //! Live inline so the private helpers are reachable without a
    //! re-exported `test_api`.
    //!
    //! Dropped (3 tests target deleted production code):
    //! - `check_valid_insn_index_zero_is_tail_call`
    //! - `check_invalid_insn_index_nonzero_returns_error`
    //! - `check_inside_function_any_insn_index_is_not_tail_call`
    //!
    //! These pinned the now-removed `is_branch_tail_call` (the
    //! insn-index-validating variant); the check is enforced inline in
    //! `process_new_insn` today.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_sign_loss)]

    use rsleigh::mem_readers::BufMemReader;
    use rsleigh::{Vn, VnSpace};
    use strider_target::SleighArch;

    use super::*;
    use crate::CfgOptions;

    type TestReader = BufMemReader<Vec<u8>>;

    fn addr_at(machine: u64, insn: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: insn,
        }
    }

    fn fake_insn() -> rsleigh::Insn {
        rsleigh::Insn {
            opcode: rsleigh::Opcode::Copy,
            output: None,
            inputs: vec![].into(),
        }
    }

    fn fake_lift_res(n: usize) -> rsleigh::LiftRes {
        fake_lift_res_with_len(n, 1)
    }

    fn fake_lift_res_with_len(n: usize, machine_insn_len: usize) -> rsleigh::LiftRes {
        rsleigh::LiftRes {
            insns: (0..n).map(|_| fake_insn()).collect(),
            machine_insn_len,
        }
    }

    fn make_sleigh() -> rsleigh::Sleigh<TestReader> {
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .expect("create empty Sleigh")
    }

    fn make_builder<'a>(
        start_addr: u64,
        sleigh: &'a mut rsleigh::Sleigh<TestReader>,
    ) -> Builder<'a, TestReader> {
        make_builder_opts(start_addr, sleigh, &CfgOptions::default())
    }

    fn make_builder_opts<'a>(
        start_addr: u64,
        sleigh: &'a mut rsleigh::Sleigh<TestReader>,
        options: &CfgOptions,
    ) -> Builder<'a, TestReader> {
        let arch = SleighArch::x86_64();
        Builder::for_arch(&arch, sleigh, start_addr, options)
    }

    fn make_region_builder<'b, 'a: 'b>(
        b: &'b mut Builder<'a, TestReader>,
        start: PcodeInsnAddr,
    ) -> RegionBuilder<'b, 'a, TestReader> {
        RegionBuilder::new(b, start, None)
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

    // ── decode_branch_target ─────────────────────────────────────────────

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
        let target = rb.decode_branch_target(vn, addr_at(0x1000, 4), &lift).unwrap();
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
        let target = rb.decode_branch_target(vn, addr_at(0x1000, 0), &lift).unwrap();
        assert_eq!(target, addr_at(0x1004, 0));
    }

    #[test]
    fn decode_branch_target_const_space_index_past_pcode_count_errors() {
        let mut sleigh = make_sleigh();
        let mut b = make_builder(0x1000, &mut sleigh);
        let rb = make_region_builder(&mut b, addr_at(0x1000, 0));
        let pcode_count = 4u64;
        let lift = fake_lift_res(usize::try_from(pcode_count).unwrap());
        let vn = Vn {
            addr_off: pcode_count + 2,
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

    // ── next_pcode_addr ──────────────────────────────────────────────────

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

    // ── is_branch_tail_call_nocheck ──────────────────────────────────────

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

    // ── process_new_insn / process_insn / finish_current_region ──────────
    //
    // Ported from pre-rewrite crates/cfg/tests/{region_builder_process,
    // region_terminator}.rs.  The pre-rewrite suite carried more tests
    // (fall-through hot-wire / push_insn helper paths); they require a
    // `TestRegionBuilder::with_parent_edge` adapter that was scoped to
    // the test_api module and isn't reintroduced here.  The subset
    // ported below exercises the per-opcode finish paths and the
    // empty-inputs error checks — the core process_new_insn contract.

    fn make_sleigh_with_bytes(bytes: Vec<u8>, base: u64) -> rsleigh::Sleigh<TestReader> {
        let arch = SleighArch::x86_64();
        let reader = BufMemReader::new(bytes, base);
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .expect("create x86_64 Sleigh")
    }

    fn make_builder_with_bytes<'a>(
        sleigh: &'a mut rsleigh::Sleigh<TestReader>,
        start: u64,
    ) -> Builder<'a, TestReader> {
        let arch = SleighArch::x86_64();
        Builder::for_arch(&arch, sleigh, start, &CfgOptions::default())
    }

    fn lift_at(bytes: Vec<u8>, base: u64, at: u64) -> rsleigh::LiftRes {
        make_sleigh_with_bytes(bytes, base)
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
        let mut sleigh = make_sleigh_with_bytes(bytes, base);
        let mut b = make_builder_with_bytes(&mut sleigh, base);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb.process_new_insn(&first, addr_at(base, 0), &lift).unwrap();
        assert_eq!(res, InsnOutcome::Continue);
        assert_eq!(rb.insns.len(), 1);
    }

    #[test]
    fn return_ends_region() {
        let base = 0x1000u64;
        let bytes = vec![0xc3u8];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, ret_insn) = find_pcode(&lift, rsleigh::Opcode::Return);
        let mut sleigh = make_sleigh_with_bytes(bytes, base);
        let mut b = make_builder_with_bytes(&mut sleigh, base);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb.process_new_insn(&ret_insn, addr_at(base, pos), &lift).unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].terminator, RegionTerminator::Return);
    }

    #[test]
    fn branch_indirect_defers_via_unresolved_indirect_branch() {
        // `jmp rax`: tier-1 cannot prove the target without an
        // installed indirect resolver.  process_new_insn must defer
        // via UnresolvedIndirectBranch rather than error.
        let base = 0x1000u64;
        let bytes = vec![0xffu8, 0xe0]; // jmp rax
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, indirect) = find_pcode(&lift, rsleigh::Opcode::BranchIndirect);
        let mut sleigh = make_sleigh_with_bytes(bytes, base);
        let mut b = make_builder_with_bytes(&mut sleigh, base);
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
        // `je +0; ret; ret`
        let base = 0x1000u64;
        let bytes = vec![0x74u8, 0x00, 0xc3, 0xc3];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, cbr) = find_pcode(&lift, rsleigh::Opcode::CondBranch);
        let mut sleigh = make_sleigh_with_bytes(bytes, base);
        let mut b = make_builder_with_bytes(&mut sleigh, base);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb.process_new_insn(&cbr, addr_at(base, pos), &lift).unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        // The taken successor's address is recorded on the terminator.  `je +0`
        // at 0x1000 targets 0x1002 — which is also the fall-through, so this is
        // the degenerate both-arms-same-address case.
        match regions[0].terminator {
            RegionTerminator::CondBranch { true_target } => {
                assert_eq!(true_target, addr_at(0x1002, 0));
            }
            ref other => panic!("expected CondBranch, got {other:?}"),
        }

        // Both successors enqueued, both wired (unweighted) to this region.
        assert_eq!(
            b.work_queue.len(),
            2,
            "CondBranch must enqueue both true and false targets"
        );
        let region_id = b.region_graph.node_indices().next().unwrap();
        for (parent, target) in &b.work_queue {
            assert_eq!(*parent, Some(region_id), "successor wired to the cond-branch region");
            assert_eq!(*target, addr_at(0x1002, 0));
        }
    }

    #[test]
    fn finish_with_branch_terminator_to_distinct_target() {
        // `jmp +1` -> target 0x1003 (distinct from natural fallthrough 0x1002).
        let base = 0x1000u64;
        let bytes = vec![0xebu8, 0x01, 0xc3];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
        let mut sleigh = make_sleigh_with_bytes(bytes, base);
        let mut b = make_builder_with_bytes(&mut sleigh, base);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb.process_new_insn(&branch, addr_at(base, pos), &lift).unwrap();
        assert_eq!(res, InsnOutcome::RegionClosed);

        let regions: Vec<&Region> = b.region_graph.node_weights().collect();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].terminator, RegionTerminator::Unconditional);
    }

    #[test]
    fn finish_with_tail_call_terminator_targets_below_start() {
        // `jmp -10` from 0x1000 -> target 0x0ff8 (below function start).
        let base = 0x1000u64;
        #[allow(clippy::cast_sign_loss)]
        let bytes = vec![0xebu8, -10_i8 as u8, 0xc3];
        let lift = lift_at(bytes.clone(), base, base);
        let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
        let mut sleigh = make_sleigh_with_bytes(bytes, base);
        let mut b = make_builder_with_bytes(&mut sleigh, base);
        let mut rb = make_region_builder(&mut b, addr_at(base, 0));

        let res = rb.process_new_insn(&branch, addr_at(base, pos), &lift).unwrap();
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
            RegionTerminator::TailCall { target: 0x0ff8 }
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
        assert!(err.to_string().contains("has no instructions"), "got: {err}");
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

