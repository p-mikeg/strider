use petgraph::graph::NodeIndex;

use super::Builder;
use crate::cfg::types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction,
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

/// Outcome of processing a single pcode instruction in [`RegionBuilder`].
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
}

impl<R: rsleigh::MemReader> RegionBuilder<'_, R> {
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
            // Relative branch: signed offset from the current pcode insn index.
            // rsleigh encodes CONST-space branch targets as two's-complement in a
            // u64, so a backward pcode-local branch comes in as `(-n) as u64`.
            // The resulting index must land within the current machine
            // instruction's pcode sequence: `0 <= target < lift_res.insns.len()`.
            // An out-of-range index would otherwise be silently skipped by the
            // build loop, advancing to the next machine instruction and
            // producing a wrong CFG with no diagnostic.
            rsleigh::VnSpace::CONST => {
                // CONST-space encodes the pcode-target offset as a
                // two's-complement i64 stored in a u64 (so a backward branch
                // arrives as `(-n) as u64`). `cast_signed` is the bit-pattern-
                // preserving u64→i64 reinterpretation; `checked_add_signed`
                // catches either-direction overflow on the index addition.
                let off = branch_target_var.addr.off.cast_signed();
                let target = branch_insn_addr.insn_index.checked_add_signed(off).ok_or(
                    ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr),
                )?;
                // The resulting index must land within the current machine
                // instruction's pcode sequence: `target < lift_res.insns.len()`.
                // An out-of-range index would otherwise be silently skipped by
                // the build loop, producing a wrong CFG with no diagnostic.
                let pcode_count = u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX);
                if target >= pcode_count {
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
                let region = self.finish_current_region(is_tail_call)?;
                if !is_tail_call {
                    // Not a tail call — enqueue the target so the builder explores it next.
                    self.builder
                        .work_queue
                        .push((Some((region, RegionEdgeKind::Branch)), branch_target_addr));
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
                let region = self.finish_current_region(false)?;

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
                let _region = self.finish_current_region(false)?;
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
    fn finish_current_region(&mut self, ends_with_tail_call: bool) -> Result<NodeIndex> {
        let region = self.builder.add_region(Region {
            start_addr: self.start_addr,
            insns: std::mem::take(&mut self.insns),
            ends_with_tail_call,
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
            let region = self.finish_current_region(false)?;
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

    use super::{ProcessInsnRes as InnerProcessInsnRes, RegionBuilder};
    use crate::cfg::types::{PcodeInsnAddr, RegionEdgeKind, RegionInstruction};
    use crate::cfg::Builder;
    use crate::error::Result;
    use petgraph::graph::NodeIndex;

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

    /// Mirror of `ProcessInsnRes` for test consumers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ProcessInsnRes {
        FinishedProcessing,
        DidntFinishProcessing,
    }

    impl From<InnerProcessInsnRes> for ProcessInsnRes {
        fn from(inner: InnerProcessInsnRes) -> Self {
            match inner {
                InnerProcessInsnRes::FinishedProcessing => ProcessInsnRes::FinishedProcessing,
                InnerProcessInsnRes::DidntFinishProcessing => ProcessInsnRes::DidntFinishProcessing,
            }
        }
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
            self.inner.process_new_insn(insn, at, lift).map(Into::into)
        }

        /// # Errors
        /// Propagates errors from the underlying instruction-processing path.
        pub fn process_insn(
            &mut self,
            insn: &rsleigh::Insn,
            at: PcodeInsnAddr,
            lift: &rsleigh::LiftRes,
        ) -> Result<ProcessInsnRes> {
            self.inner.process_insn(insn, at, lift).map(Into::into)
        }

        /// # Errors
        /// Returns [`ErrorKind::EmptyRegion`] if the region has no instructions.
        pub fn finish_current_region(
            &mut self,
            ends_with_tail_call: bool,
        ) -> Result<NodeIndex> {
            self.inner.finish_current_region(ends_with_tail_call)
        }
    }
}

