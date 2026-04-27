use petgraph::graph::NodeIndex;

use super::Builder;
use crate::cfg::types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction, RegionTerminator,
};
use crate::error::{ErrorKind, Result};

/// Returns the [`PcodeInsnAddr`] that comes immediately after `addr` within
/// the lifted machine instruction `lift_res`.
///
/// - If `addr.insn_index + 1` is still within `lift_res.insns`, returns the
///   same machine address with `insn_index` advanced by one.
/// - Otherwise returns the start (`insn_index = 0`) of the *next* machine
///   instruction.
///
/// # Errors
/// Returns [`ErrorKind::MachineAddrOverflow`] when the current machine
/// address plus `lift_res.machine_insn_len` overflows `u64`.
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
        .ok_or(ErrorKind::MachineAddrOverflow(addr))?;
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

        match branch_target_var.addr.space {
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
                let raw = branch_target_var.addr.off;
                let off: i64 = match branch_target_var.size {
                    1 => (raw as i8) as i64,
                    2 => (raw as i16) as i64,
                    4 => (raw as i32) as i64,
                    _ => raw.cast_signed(),
                };
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or(
                    ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr),
                )?;
                let pcode_count = u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX);
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
                    return Err(ErrorKind::InvalidBranchTargetVaErr(
                        branch_target_var,
                        branch_insn_addr,
                    )
                    .into());
                }
                Ok(PcodeInsnAddr {
                    machine_addr: branch_insn_addr.machine_addr,
                    insn_index: target,
                })
            }
            // Absolute branch: the offset IS the target machine address
            space if space == default_code_space => Ok(PcodeInsnAddr {
                machine_addr: MachineInsnAddr {
                    addr: branch_target_var.addr.off,
                },
                insn_index: 0,
            }),
            _ => {
                Err(ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr).into())
            }
        }
    }

    /// Checks whether `branch_target_addr` should be treated as a tail call
    /// using only address-bounds reasoning (no `insn_index` validation).
    ///
    /// A branch is a tail call if:
    /// - Its target lies *before* the function start AND
    ///   `allow_code_before_start_addr` is `false`, **OR**
    /// - `fn_max_size` is set AND the target lies at or beyond
    ///   `start_addr + fn_max_size`.
    pub(super) fn is_branch_tail_call_nocheck(&self, branch_target_addr: PcodeInsnAddr) -> bool {
        // Only the machine insn address matters for bounds checking; the pcode
        // insn index is irrelevant here.
        let addr = branch_target_addr.machine_addr;

        if addr < self.builder.start_addr && !self.builder.options.allow_code_before_start_addr {
            return true;
        }

        if let Some(fn_max_size) = self.builder.options.fn_max_size {
            // Saturate on overflow: if start + max would exceed u64::MAX, no target can
            // be at-or-beyond the sum, so the only way `addr >= sat_sum` is when
            // `sat_sum == u64::MAX && addr == u64::MAX`. That tiny boundary case is
            // the correct semantics — an address past the end of addressable memory
            // is, by definition, outside any reasonable function.
            let end_exclusive = self.builder.start_addr.addr.saturating_add(fn_max_size);
            if end_exclusive <= addr.addr {
                return true;
            }
        }
        false
    }

    /// Determines whether `branch_target_addr` is a tail call, validating the
    /// pcode insn index.
    ///
    /// A well-formed tail call must target the *first* pcode instruction of a
    /// machine instruction (`insn_index == 0`).  A branch whose address bounds
    /// indicate a tail call but whose `insn_index != 0` is malformed and
    /// returns [`ErrorKind::InvalidTailCall`].
    pub(super) fn is_branch_tail_call(&self, branch_target_addr: PcodeInsnAddr) -> Result<bool> {
        let is_tail_call = self.is_branch_tail_call_nocheck(branch_target_addr);

        if is_tail_call {
            // Tail calls may only jump to the start of a machine insn. They
            // cannot target a specific pcode op inside a machine insn.
            if branch_target_addr.insn_index != 0 {
                return Err(ErrorKind::InvalidTailCall(branch_target_addr).into());
            }
        }

        Ok(is_tail_call)
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
                    .ok_or(ErrorKind::MissingBranchTarget(addr))?;
                let branch_target_addr = self.decode_branch_target(target_var, addr, lift_res)?;
                let is_tail_call = self.is_branch_tail_call(branch_target_addr)?;
                // BUG-25: clang at -O0 (used for the aarch64be / ppc32le
                // fixtures, where no Debian gcc cross exists) emits
                // explicit unconditional `b <next-instr>` between adjacent
                // basic blocks instead of letting control fall through.
                // Without normalisation every such transition shows up as
                // a `Branch` edge and the CFG never has any `Fallthrough`
                // edges, breaking downstream passes / queries that
                // distinguish the two.  When the branch target is exactly
                // the address that decoding would naturally advance to
                // next (`next_pcode_addr(addr, lift_res)`) AND is the
                // start of a machine instruction (`insn_index == 0`),
                // classify the edge as `Fallthrough`.  Restricting to
                // machine-instruction boundaries avoids reclassifying any
                // intra-machine-instruction p-code `Branch` whose target
                // happens to be the next p-code op in the same insn.
                // This is an edge-classification change only — the target
                // is still enqueued for exploration the same way.
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
                    .ok_or(ErrorKind::MissingBranchTarget(addr))?;
                let target_addr = self.decode_branch_target(target_var, addr, lift_res)?;

                // We reached the end of the current region
                let region = self.finish_current_region(RegionTerminator::CondBranch)?;

                // Add the true case
                self.builder
                    .work_queue
                    .push((Some((region, RegionEdgeKind::IfCaseTrue)), target_addr));
                let next_insn_addr = next_pcode_addr(addr, lift_res)?;

                // Add the false case
                self.builder
                    .work_queue
                    .push((Some((region, RegionEdgeKind::IfCaseFalse)), next_insn_addr));
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            rsleigh::Opcode::Return => {
                self.finish_current_region(RegionTerminator::Return)?;
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            rsleigh::Opcode::BranchIndirect => {
                // Phase 5: dispatch into the lazy mini-graph resolver.
                // The resolver folds the region's value-producing pcode
                // insns into an isolated IR graph and inspects the
                // producer of `target_vn` after constant folding.
                // - `Single(K)` inside the function range → intra-fn
                //   `Branch` to K (enqueue successor for exploration).
                // - `Single(K)` outside the function range →
                //   `TailCall { target: K }` (no successor edge).
                // - `LinkRegister` → `Return` (no successor edge).
                // - unresolvable → propagate
                //   [`ErrorKind::UnresolvedIndirectBranch`].
                //
                // `CallIndirect` is intentionally NOT routed here — it
                // remains a non-terminator opcode handled by the IR
                // layer.  See plan
                // `2026-04-27-indirect-branch-resolution.md` Phase 5.
                let target_vn = *insn
                    .inputs
                    .first()
                    .ok_or(ErrorKind::MissingBranchTarget(addr))?;
                let resolved = super::indirect_resolve::resolve_indirect_target(
                    &self.insns,
                    target_vn,
                    &self.builder.sleigh,
                    self.builder.options.link_register_vn,
                    self.builder.options.read_only_memory.as_deref(),
                    addr,
                    self.builder.endianness,
                )?;
                match resolved {
                    super::indirect_resolve::ResolvedTargets::LinkRegister => {
                        self.finish_current_region(RegionTerminator::Return)?;
                    }
                    super::indirect_resolve::ResolvedTargets::Single(target) => {
                        let target_addr = PcodeInsnAddr {
                            machine_addr: MachineInsnAddr { addr: target },
                            insn_index: 0,
                        };
                        if self.is_branch_tail_call(target_addr)? {
                            self.finish_current_region(
                                RegionTerminator::TailCall { target },
                            )?;
                        } else {
                            // Intra-fn target — finish region with
                            // `Branch` and enqueue the successor for
                            // exploration.
                            let region = self
                                .finish_current_region(RegionTerminator::Branch)?;
                            self.builder.work_queue.push((
                                Some((region, RegionEdgeKind::Branch)),
                                target_addr,
                            ));
                        }
                    }
                    super::indirect_resolve::ResolvedTargets::Multiple(_) => {
                        // Reserved for the future jump-table resolver;
                        // not produced this round.  Surface as
                        // unresolved so callers see a real error rather
                        // than a silent miscompilation.
                        return Err(
                            ErrorKind::UnresolvedIndirectBranch(addr).into()
                        );
                    }
                }
                Ok(ProcessInsnRes::FinishedProcessing)
            }
            _ => Ok(ProcessInsnRes::DidntFinishProcessing),
        }
    }

    /// Finalises the region that has been accumulating instructions.
    ///
    /// Calls `Builder::add_region` (which enforces non-emptiness via
    /// [`ErrorKind::EmptyRegion`]) and, if there is a parent edge, adds that
    /// edge to the graph. Returns the new region's [`NodeIndex`].
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

    /// Processes `insn` at `addr`, first checking whether `addr` is already
    /// the start of a known region.
    ///
    /// If so, the current region has fallen through into an already-explored
    /// region: the current region is finalised and a
    /// [`RegionEdgeKind::Fallthrough`] edge is added to the existing region.
    /// Otherwise delegates to [`process_new_insn`](Self::process_new_insn).
    fn process_insn(
        &mut self,
        insn: &rsleigh::Insn,
        addr: PcodeInsnAddr,
        lift_res: &rsleigh::LiftRes,
    ) -> Result<ProcessInsnRes> {
        // If `addr` is the start of an already-explored region, the current region
        // fell through to it: finalise the current region and add a Fallthrough edge.
        if let Some(&existing_region_id) = self.builder.start_addr_to_region_id.get(&addr) {
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
            let lift_res = self
                .builder
                .sleigh
                .lift_one(cur_addr.machine_addr.addr)
                .map_err(|e| ErrorKind::GenericSleighError(format!("{e:?}")))?;
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
    use crate::error::Result;
    use petgraph::graph::NodeIndex;

    pub use super::ProcessInsnRes;

    /// Test-only forwarder for the private free function `next_pcode_addr`.
    ///
    /// # Errors
    /// Returns [`crate::ErrorKind::MachineAddrOverflow`] when advancing past
    /// the last pcode op overflows the 64-bit machine-address space.
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
        /// Propagates errors from the underlying tail-call check.
        pub fn is_branch_tail_call(&self, target: PcodeInsnAddr) -> Result<bool> {
            self.inner.is_branch_tail_call(target)
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
        /// Returns [`crate::ErrorKind::EmptyRegion`] if the region has no
        /// instructions.
        pub fn finish_current_region(
            &mut self,
            terminator: RegionTerminator,
        ) -> Result<NodeIndex> {
            self.inner.finish_current_region(terminator)
        }
    }
}

