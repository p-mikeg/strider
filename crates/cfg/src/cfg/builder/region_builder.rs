use std::collections::VecDeque;

use petgraph::graph::NodeIndex;

use super::Builder;
use crate::cfg::types::{
    MachineInsnAddr, PcodeInsnAddr, Region, RegionEdgeKind, RegionInstruction,
};
use crate::error::{ErrorKind, Result};

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
/// Created internally by `Builder::explore_new_region`; not part of the
/// public API.  Holds a mutable reference back to the parent [`Builder`] so
/// it can enqueue successor regions and call `Builder::add_region`.
pub(super) struct RegionBuilder<'a, R: rsleigh::MemReader> {
    /// Parent builder — used to access the Sleigh context, options, graph,
    /// and work queue.
    pub(super) builder: &'a mut Builder<R>,
    /// Address of the first instruction this region will contain.
    pub(super) start_addr: PcodeInsnAddr,
    /// Instructions accumulated so far.
    pub(super) insns: VecDeque<RegionInstruction>,
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
            insns: VecDeque::new(),
            parent_edge,
        }
    }
}

impl<R: rsleigh::MemReader> RegionBuilder<'_, R> {
    /// Decodes a pcode branch-target varnode into a [`PcodeInsnAddr`].
    ///
    /// Pcode encodes branch targets in two ways:
    /// - **Relative** (`VnSpace::CONST`): the target is a pcode-instruction
    ///   index *offset* within the same machine instruction.
    /// - **Absolute** (default code space): the target is a raw virtual
    ///   address; the pcode index is implicitly 0 (start of machine insn).
    fn decode_branch_target(
        &self,
        branch_target_var: rsleigh::Vn,
        branch_insn_addr: PcodeInsnAddr,
    ) -> Result<PcodeInsnAddr> {
        let default_code_space = self.builder.sleigh.default_code_space();

        match branch_target_var.addr.space {
            // Relative branch: signed offset from the current pcode insn index.
            // rsleigh encodes CONST-space branch targets as two's-complement in a
            // u64, so a backward pcode-local branch comes in as `(-n) as u64`.
            rsleigh::VnSpace::CONST => {
                let base = branch_insn_addr.insn_index as i64;
                let off = branch_target_var.addr.off as i64;
                let target = base.checked_add(off).ok_or(
                    ErrorKind::InvalidBranchTargetVaErr(branch_target_var, branch_insn_addr),
                )?;
                if target < 0 {
                    return Err(ErrorKind::InvalidBranchTargetVaErr(
                        branch_target_var,
                        branch_insn_addr,
                    )
                    .into());
                }
                Ok(PcodeInsnAddr {
                    machine_addr: branch_insn_addr.machine_addr,
                    insn_index: target as u64,
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
    /// using only address-bounds reasoning (no insn_index validation).
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
        self.insns.push_back(RegionInstruction {
            addr,
            insn: insn.to_owned(),
        });

        match insn.opcode {
            rsleigh::Opcode::Branch => {
                let branch_target_addr = self.decode_branch_target(insn.inputs[0], addr)?;
                if self.is_branch_tail_call(branch_target_addr)? {
                    // The tail call marks the end of control flow for this specific path.
                    let _region = self.finish_current_region(true)?;
                    Ok(ProcessInsnRes::FinishedProcessing)
                } else {
                    // We reached the end of the current bb but we know the next address to jump to so enqueue it
                    let region = self.finish_current_region(false)?;
                    self.builder
                        .work_queue
                        .push((Some((region, RegionEdgeKind::Branch)), branch_target_addr));
                    Ok(ProcessInsnRes::FinishedProcessing)
                }
            }
            rsleigh::Opcode::CondBranch => {
                let target_addr = self.decode_branch_target(insn.inputs[0], addr)?;

                // We reached the end of the current region
                let region = self.finish_current_region(false)?;

                // Add the true case
                self.builder
                    .work_queue
                    .push((Some((region, RegionEdgeKind::IfCaseTrue)), target_addr));
                // The false case requires calculation of the next instruction (is it in the current pcode instr or the next one)
                let next_insn_addr = if addr.insn_index + 1 == lift_res.insns.len() as u64 {
                    PcodeInsnAddr {
                        machine_addr: MachineInsnAddr {
                            addr: addr.machine_addr.addr + lift_res.machine_insn_len as u64,
                        },
                        insn_index: 0,
                    }
                } else {
                    PcodeInsnAddr {
                        machine_addr: addr.machine_addr,
                        insn_index: addr.insn_index + 1,
                    }
                };

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
    /// Calls `Builder::add_region` and, if there is a parent edge, adds
    /// that edge to the graph.  Returns the new region's [`NodeIndex`].
    fn finish_current_region(&mut self, ends_with_tail_call: bool) -> Result<NodeIndex> {
        if self.insns.is_empty() {
            return Err(ErrorKind::NoInstructionsRegionBuilder.into());
        }
        let region = self.builder.add_region(Region {
            start_addr: self.start_addr,
            insns: self.insns.to_owned(),
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
        let existing_region = self.builder.start_addr_to_region_id.get(&addr);
        // If we already processed the instruction - we fell through to an already processed region
        if let Some(region_id) = existing_region {
            let region_id = *region_id;
            // The parent region falls through to this region
            let region = self.finish_current_region(false)?;
            self.builder
                .graph
                .add_edge(region, region_id, RegionEdgeKind::Fallthrough);
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
    /// iteration.  We must add that base offset to the `enumerate` counter so
    /// that every `RegionInstruction` carries the correct
    /// `(machine_addr, insn_index)` pair.  Subsequent machine instructions
    /// always start at pcode index 0, so the offset resets naturally.
    pub(super) fn build(mut self) -> Result<()> {
        let mut cur_addr = self.start_addr;
        loop {
            let lift_res = self
                .builder
                .sleigh
                .lift_one(cur_addr.machine_addr.addr)
                .map_err(|e| ErrorKind::GenericSleighError(format!("{:?}", e)))?;
            // Save the starting pcode index for this machine instruction.
            // For the first machine instruction this may be non-zero when the
            // work queue delivered a mid-instruction entry point.  For all
            // subsequent machine instructions it is always 0.
            let start_pcode_idx = cur_addr.insn_index;
            for (i, insn) in lift_res
                .insns
                .iter()
                .skip(start_pcode_idx as usize)
                .enumerate()
            {
                cur_addr = PcodeInsnAddr {
                    machine_addr: cur_addr.machine_addr,
                    insn_index: start_pcode_idx + i as u64,
                };

                let res = self.process_insn(insn, cur_addr, &lift_res)?;
                if matches!(res, ProcessInsnRes::FinishedProcessing) {
                    return Ok(());
                }
            }
            // We're done exploring a single machine insn, continue to the next one
            cur_addr = PcodeInsnAddr {
                machine_addr: MachineInsnAddr {
                    addr: cur_addr.machine_addr.addr + (lift_res.machine_insn_len as u64),
                },
                insn_index: 0,
            };
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
    use std::collections::VecDeque;

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
                    insns: VecDeque::new(),
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
                    insns: VecDeque::new(),
                    parent_edge: Some(parent),
                },
            }
        }

        /// Returns the accumulated instructions for this region.
        #[must_use]
        pub fn insns(&self) -> &VecDeque<RegionInstruction> {
            &self.inner.insns
        }

        /// Pushes an instruction onto the back of the instruction queue.
        pub fn push_insn(&mut self, insn: RegionInstruction) {
            self.inner.insns.push_back(insn);
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
        /// Propagates errors from the underlying branch-target decode.
        pub fn decode_branch_target(
            &self,
            vn: rsleigh::Vn,
            at: PcodeInsnAddr,
        ) -> Result<PcodeInsnAddr> {
            self.inner.decode_branch_target(vn, at)
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
        /// Returns `NoInstructionsRegionBuilder` if the region has no instructions.
        pub fn finish_current_region(
            &mut self,
            ends_with_tail_call: bool,
        ) -> Result<NodeIndex> {
            self.inner.finish_current_region(ends_with_tail_call)
        }
    }
}

