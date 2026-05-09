use petgraph::graph::NodeIndex;

use super::Builder;
use crate::cfg::types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction, RegionTerminator,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessInsnRes {
    /// The instruction terminated the current region (branch, return, or
    /// fall-through into an already-existing region).
    FinishedProcessing,
    /// The instruction did not terminate the region; decoding continues.
    DidntFinishProcessing,
}

/// Builds a single [`Region`] by decoding pcode instructions one at a time.
///
/// Created internally by `Builder::explore`; not part of the public API.
/// Holds a mutable reference back to the parent [`Builder`] so it can
/// enqueue successor regions and call `Builder::add_region`.
pub(super) struct RegionBuilder<'a, R: rsleigh::MemReader> {
    /// Parent builder — used to access the Sleigh context, options, graph,
    /// and work queue.
    pub(super) builder: &'a mut Builder<R>,
    /// Address of the first instruction this region will contain.
    pub(super) start_addr: PcodeInsnAddr,
    /// Instructions accumulated so far.
    pub(super) insns: Vec<RegionInstruction>,
    /// The edge from the predecessor region to this one, if any.
    /// `None` only for the function entry region.
    pub(super) parent_edge: Option<(NodeIndex, RegionEdgeKind)>,
}

impl<'a, R: rsleigh::MemReader> RegionBuilder<'a, R> {
    pub(super) fn new(
        builder: &'a mut Builder<R>,
        start_addr: PcodeInsnAddr,
        parent_edge: Option<(NodeIndex, RegionEdgeKind)>,
    ) -> Self {
        RegionBuilder {
            builder,
            start_addr,
            insns: Vec::new(),
            parent_edge,
        }
    }

    /// Lift a single machine instruction at `addr`, consulting the
    /// optional [`crate::DecodeCache`] when present.  Returns an
    /// `Arc<LiftRes>` so successive callers at the same address
    /// (across CFG rebuilds within one `strider::run`) share the
    /// underlying decoded pcode without re-invoking Sleigh.
    fn lift_one_cached(&mut self, addr: u64) -> Result<std::sync::Arc<rsleigh::LiftRes>> {
        if let Some(cache) = &self.builder.decode_cache
            && let Some(arc) = cache.get(addr)
        {
            return Ok(arc);
        }
        let res = self
            .builder
            .sleigh
            .lift_one(addr)
            .map_err(|e| anyhow!("generic sleigh error {e:?}"))?;
        let arc = std::sync::Arc::new(res);
        if let Some(cache) = &self.builder.decode_cache {
            cache.insert(addr, std::sync::Arc::clone(&arc));
        }
        Ok(arc)
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
    /// Appends the instruction to the current region, then acts on the opcode:
    /// - `Branch`: classifies as tail call or enqueues the jump target.
    /// - `CondBranch`: enqueues both the taken and not-taken successors.
    /// - `Return`: ends the region.
    /// - Everything else: returns [`ProcessInsnRes::DidntFinishProcessing`].
    fn process_new_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<ProcessInsnRes> {
        self.insns.push(RegionInstruction {
            addr,
            insn: insn.clone(),
        });

        match insn.opcode {
            rsleigh::Opcode::Branch => {
                let target_var = *insn
                    .inputs
                    .first()
                    .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))?;
                let branch_target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
                let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);
                if is_tail_call && branch_target_addr.insn_index != 0 {
                    bail!("invalid tail call at opcode {branch_target_addr:?}");
                }
                // clang at -O0 (used for the aarch64be / ppc32le
                // fixtures, where no Debian gcc cross exists) emits
                // explicit unconditional `b <next-instr>` between
                // adjacent basic blocks instead of letting control
                // fall through.  Without normalisation every such
                // transition shows up as a `Branch` edge and the CFG
                // never has any `Fallthrough` edges, breaking
                // downstream passes / queries that distinguish the
                // two.  When the branch target is exactly the address
                // that decoding would naturally advance to next AND
                // is the start of a machine instruction (`insn_index
                // == 0`), classify the edge as `Fallthrough`.
                // Restricting to machine-instruction boundaries
                // avoids reclassifying any intra-machine-instruction
                // p-code `Branch` whose target happens to be the
                // next p-code op in the same insn.  This is an
                // edge-classification change only — the target is
                // still enqueued for exploration the same way.
                let edge_kind = if !is_tail_call
                    && branch_target_addr.insn_index == 0
                    && next_pcode_addr(addr, lift_res)
                        .is_ok_and(|next| next == branch_target_addr)
                {
                    RegionEdgeKind::Fallthrough
                } else {
                    RegionEdgeKind::Branch
                };
                let terminator = if is_tail_call {
                    RegionTerminator::TailCall {
                        target: branch_target_addr.machine_addr.addr,
                    }
                } else {
                    RegionTerminator::Branch
                };
                let region = self.finish_current_region(terminator)?;
                if !is_tail_call {
                    // Not a tail call — enqueue the target so the builder explores it next.
                    self.builder
                        .work_queue
                        .push((Some((region, edge_kind)), branch_target_addr));
                }
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            rsleigh::Opcode::CondBranch => {
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
                        // Both in-range — original CondBranch behaviour.
                        let region =
                            self.finish_current_region(RegionTerminator::CondBranch)?;
                        self.builder.work_queue.push((
                            Some((region, RegionEdgeKind::IfCaseTrue)),
                            target_addr,
                        ));
                        self.builder.work_queue.push((
                            Some((region, RegionEdgeKind::IfCaseFalse)),
                            next_insn_addr,
                        ));
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
                        // `RegionTerminator::Branch` to the in-range
                        // successor.  The conditional is lost, but the
                        // lift completes.
                        //
                        // If popping leaves the region empty (the function
                        // body is exactly the conditional jump and nothing
                        // else), fall back to `TailCall` to the in-range
                        // target so `add_region`'s non-empty invariant
                        // holds.  This degenerate case loses the in-range
                        // edge entirely, but it is essentially unobserved
                        // in real binaries.
                        let in_range = if true_oob { next_insn_addr } else { target_addr };
                        // Pop the trailing CondBranch from `self.insns`
                        // so the IR's per-region loop does not re-route
                        // it through `handle_cond_branch` (which would
                        // fail looking up the missing OOB edge).  Even
                        // when this leaves the region empty
                        // (single-instruction case), `add_region` now
                        // accepts empty regions terminated with Branch.
                        // The IR-layer per-region driver iterates
                        // `region.insns` (a no-op for empty insns) and
                        // handles the Branch terminator + outgoing edge
                        // separately, so the in-range successor is
                        // preserved as a regular intra-function branch.
                        self.insns.pop();
                        let region = self.finish_current_region(RegionTerminator::Branch)?;
                        self.builder
                            .work_queue
                            .push((Some((region, RegionEdgeKind::Branch)), in_range));
                    }
                }
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            rsleigh::Opcode::Return => {
                self.finish_current_region(RegionTerminator::Return)?;
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            rsleigh::Opcode::BranchIndirect => self.process_branch_indirect(insn, addr),
            rsleigh::Opcode::CallOther => {
                // Resolve the user-op id from the CONST input at
                // position 0.  Fall through to today's behaviour if
                // the input shape is unexpected — the IR layer's
                // strict-on-emission check will surface any real
                // problem with full context.
                let id_vn = match insn.inputs.first() {
                    Some(v) => v,
                    None => return Ok(ProcessInsnRes::DidntFinishProcessing),
                };
                if id_vn.addr_space != rsleigh::VnSpace::CONST {
                    return Ok(ProcessInsnRes::DidntFinishProcessing);
                }
                let id_u32 = match u32::try_from(id_vn.addr_off) {
                    Ok(v) => v,
                    Err(_) => return Ok(ProcessInsnRes::DidntFinishProcessing),
                };
                let name = self.builder.sleigh.user_op_name(id_u32);
                let preset = self.builder.preset;
                let class = name.and_then(|n| target::call_other_abi::classify(preset, n));
                if matches!(class, Some(target::call_other_abi::CallOtherClass::NoReturn)) {
                    // CallOther is already in self.insns from the
                    // process_new_insn prologue push; finish_current_region
                    // carries it.  Trailing BranchIndirect is never decoded.
                    self.finish_current_region(RegionTerminator::NoReturn)?;
                    return Ok(ProcessInsnRes::FinishedProcessing);
                }
                Ok(ProcessInsnRes::DidntFinishProcessing)
            }
            _ => Ok(ProcessInsnRes::DidntFinishProcessing),
        }
    }

    /// Handles a `BranchIndirect` opcode by classifying its target via
    /// the mini-graph resolver (or a cached `known_targets` entry from
    /// the strider orchestrator's IR-level indirect-branch resolver feedback path) and finalising
    /// the region with the matching terminator:
    /// - `Single(K)` inside the function range → `Branch` to K
    ///   (enqueue successor for exploration).
    /// - `Single(K)` outside the function range → `TailCall { target:
    ///   K }` (no successor edge).
    /// - `LinkRegister` → `Return` (no successor edge).
    /// - `Multiple` → `Switch` (one `Branch` edge per target).  If
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
    ) -> Result<ProcessInsnRes> {
        let target_vn = *insn
            .inputs
            .first()
            .ok_or_else(|| anyhow!("branch instruction at {addr:?} has no target operand"))?;
        let resolved = if let Some(cached) =
            self.builder.options.known_targets.get(&addr).cloned()
        {
            Some(cached)
        } else {
            super::indirect_resolve::resolve_indirect_target(
                &self.insns,
                target_vn,
                &self.builder.sleigh,
                self.builder.options.link_register_vn,
                self.builder.options.read_only_memory.as_deref(),
                self.builder.endianness,
            )?
        };
        // None means "I can't classify this from the current region's
        // pcode alone" — defer to the strider outer loop, which runs
        // the IR-level indirect-branch resolver on the optimised IR.
        // Stamp `target_vn` and `addr` onto the deferred terminator so
        // the strider lifter can emit a placeholder
        // `Return(target_value)` anchoring the value for IR-level
        // indirect-branch resolver inspection.  No outgoing edge.
        let Some(resolved) = resolved else {
            self.finish_current_region(
                RegionTerminator::UnresolvedIndirectBranch { target_vn, addr },
            )?;
            return Ok(ProcessInsnRes::FinishedProcessing);
        };
        match resolved {
            super::indirect_resolve::ResolvedTargets::LinkRegister => {
                self.finish_current_region(RegionTerminator::Return)?;
            }
            super::indirect_resolve::ResolvedTargets::Single(target) => {
                let target_addr = PcodeInsnAddr::at_machine_start(target);
                // `_nocheck` is sufficient: `at_machine_start` pins
                // `insn_index == 0`, so the validating variant has
                // nothing to validate.
                self.finish_branch_or_tail_call(
                    target_addr,
                    RegionEdgeKind::Branch,
                    self.is_branch_tail_call_nocheck(target_addr),
                )?;
            }
            super::indirect_resolve::ResolvedTargets::Multiple(targets) => {
                // `Multiple` is exclusively an IR-level indirect-branch
                // resolver feedback shape; the cfg-time mini-graph
                // resolver only ever returns Single / LinkRegister / None.
                let any_out_of_range = targets.iter().any(|t| {
                    self.is_branch_tail_call_nocheck(PcodeInsnAddr::at_machine_start(*t))
                });
                if any_out_of_range {
                    self.finish_current_region(
                        RegionTerminator::UnresolvedIndirectBranch { target_vn, addr },
                    )?;
                    return Ok(ProcessInsnRes::FinishedProcessing);
                }
                let region = self.finish_current_region(RegionTerminator::Switch {
                    target_vn,
                    targets: targets.clone(),
                    target_value: None,
                })?;
                for target in targets {
                    let target_addr = PcodeInsnAddr::at_machine_start(target);
                    self.builder
                        .work_queue
                        .push((Some((region, RegionEdgeKind::Branch)), target_addr));
                }
            }
        }
        Ok(ProcessInsnRes::FinishedProcessing)
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
        if let Some((parent_id, edge_kind)) = self.parent_edge {
            self.builder.graph.add_edge(parent_id, region, edge_kind);
        }
        Ok(region)
    }

    /// Either finishes the current region with `RegionTerminator::TailCall`
    /// (when `is_tail_call`) or with `RegionTerminator::Branch` plus an
    /// outgoing `edge_kind` edge to `target_addr` enqueued for further
    /// exploration.  Shared between the `Branch` opcode arm and
    /// `process_branch_indirect`'s `Single` path — both classify a single
    /// jump target the same way (intra-function vs OOB).
    fn finish_branch_or_tail_call(
        &mut self,
        target_addr: PcodeInsnAddr,
        edge_kind: RegionEdgeKind,
        is_tail_call: bool,
    ) -> Result<()> {
        if is_tail_call {
            self.finish_current_region(RegionTerminator::TailCall {
                target: target_addr.machine_addr.addr,
            })?;
        } else {
            let region = self.finish_current_region(RegionTerminator::Branch)?;
            self.builder
                .work_queue
                .push((Some((region, edge_kind)), target_addr));
        }
        Ok(())
    }

    /// Processes `insn` at `addr`, first checking whether `addr` is already
    /// the start of a known region.
    ///
    /// If so, the current region has fallen through into an already-explored
    /// region: the current region is finalised and a
    /// [`RegionEdgeKind::Fallthrough`] edge is added to the existing region.
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
    ) -> Result<ProcessInsnRes> {
        // If `addr` is the start of an already-explored region, the current region
        // fell through to it: finalise the current region and add a Fallthrough edge.
        if let Some(&existing_region_id) = self.builder.start_addr_to_region_id.get(&addr) {
            if self.insns.is_empty() {
                if let Some((parent_id, edge_kind)) = self.parent_edge {
                    self.builder
                        .graph
                        .add_edge(parent_id, existing_region_id, edge_kind);
                }
                return Ok(ProcessInsnRes::FinishedProcessing);
            }
            let region = self.finish_current_region(RegionTerminator::Fallthrough)?;
            self.builder
                .graph
                .add_edge(region, existing_region_id, RegionEdgeKind::Fallthrough);
            return Ok(ProcessInsnRes::FinishedProcessing);
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
            let lift_res = self.lift_one_cached(cur_addr.machine_addr.addr)?;
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
                if matches!(res, ProcessInsnRes::FinishedProcessing) {
                    return Ok(());
                }
            }
            // We're done exploring a single machine insn, continue to the next one
            cur_addr = next_pcode_addr(cur_addr, &lift_res)?;
            // If fall-through crossed `start + fn_max_size`, terminate the
            // region with a synthetic `TailCall { target: cur_addr }` rather
            // than continuing to lift OOB instructions.  Lifting past the
            // bound is what surfaced as `"invalid tail call at opcode ..."`
            // when a multi-pcode-op insn (e.g. `lock cmpxchg`) past the
            // bound returned a CONST-arm `PcodeInsnAddr` with non-zero
            // `insn_index` and an OOB `machine_addr`.  `next_pcode_addr`
            // only advances forward in machine address, so the upper-bound
            // check is sufficient — `is_branch_tail_call_nocheck` happens
            // to also check the lower bound, but `cur_addr.machine_addr >=
            // self.start_addr.addr` always holds here.
            //
            // Empty-`insns` guard: if every machine instruction so far
            // decoded to zero pcode ops (true NOPs on some Sleigh specs),
            // the inner `for` loop never appended to `self.insns` and
            // `add_region` would reject the empty region.  Skip the
            // truncation in that degenerate case — the next iteration
            // will keep lifting (the pre-existing zero-pcode-op gap is
            // not introduced by this fix).
            if !self.insns.is_empty() && self.is_branch_tail_call_nocheck(cur_addr) {
                self.finish_current_region(RegionTerminator::TailCall {
                    target: cur_addr.machine_addr.addr,
                })?;
                return Ok(());
            }
        }
    }
}

#[doc(hidden)]
pub mod test_api {
    //! Test-only wrapper around `RegionBuilder` so integration tests can drive
    //! its private methods directly.

    use super::RegionBuilder;
    use crate::cfg::types::{PcodeInsnAddr, RegionEdgeKind, RegionInstruction, RegionTerminator};
    use crate::cfg::Builder;
    use crate::Result;
    use petgraph::graph::NodeIndex;

    pub use super::ProcessInsnRes;

    /// Test-only forwarder for the private free function `next_pcode_addr`.
    ///
    /// # Errors
    /// Returns an error when advancing past the last pcode op overflows
    /// the 64-bit machine-address space.
    pub fn next_pcode_addr(
        addr: PcodeInsnAddr,
        lift: &rsleigh::LiftRes,
    ) -> Result<PcodeInsnAddr> {
        super::next_pcode_addr(addr, lift)
    }

    /// Owns a `RegionBuilder` for the lifetime of the test.
    pub struct TestRegionBuilder<'a, R: rsleigh::MemReader> {
        inner: RegionBuilder<'a, R>,
    }

    impl<'a, R: rsleigh::MemReader> TestRegionBuilder<'a, R> {
        /// Creates a new `TestRegionBuilder` anchored at `start_addr`.
        pub fn new(builder: &'a mut Builder<R>, start_addr: PcodeInsnAddr) -> Self {
            Self {
                inner: RegionBuilder {
                    builder,
                    start_addr,
                    insns: Vec::new(),
                    parent_edge: None,
                },
            }
        }

        /// Creates a new `TestRegionBuilder` with an explicit parent edge.
        pub fn with_parent_edge(
            builder: &'a mut Builder<R>,
            start_addr: PcodeInsnAddr,
            parent: (NodeIndex, RegionEdgeKind),
        ) -> Self {
            Self {
                inner: RegionBuilder {
                    builder,
                    start_addr,
                    insns: Vec::new(),
                    parent_edge: Some(parent),
                },
            }
        }

        /// Returns the accumulated instructions for this region.
        #[must_use]
        pub fn insns(&self) -> &[RegionInstruction] {
            &self.inner.insns
        }

        /// Pushes an instruction onto the back of the instruction queue.
        pub fn push_insn(&mut self, insn: RegionInstruction) {
            self.inner.insns.push(insn);
        }

        /// Checks whether `target` is a tail call without validating `insn_index`.
        #[must_use]
        pub fn is_branch_tail_call_nocheck(&self, target: PcodeInsnAddr) -> bool {
            self.inner.is_branch_tail_call_nocheck(target)
        }

        /// # Errors
        /// Propagates errors from the underlying branch-target decode,
        /// including out-of-range CONST-space pcode indices (target index
        /// negative or `>= lift.insns.len()`).
        pub fn decode_branch_target(
            &self,
            vn: rsleigh::Vn,
            at: PcodeInsnAddr,
            lift: &rsleigh::LiftRes,
        ) -> Result<PcodeInsnAddr> {
            self.inner.decode_branch_target(vn, at, lift)
        }

        /// # Errors
        /// Propagates errors from the underlying instruction-processing path.
        pub fn process_new_insn(
            &mut self,
            insn: &rsleigh::Insn,
            at: PcodeInsnAddr,
            lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_new_insn(insn, at, lift)
        }

        /// # Errors
        /// Propagates errors from the underlying instruction-processing path.
        pub fn process_insn(
            &mut self,
            insn: &rsleigh::Insn,
            at: PcodeInsnAddr,
            lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_insn(insn, at, lift)
        }

        /// # Errors
        /// Returns an error if the region has no instructions.
        pub fn finish_current_region(
            &mut self,
            terminator: RegionTerminator,
        ) -> Result<NodeIndex> {
            self.inner.finish_current_region(terminator)
        }
    }
}

