//! One decode, in the context this build owns: the region's ISA mode held,
//! the `noflow` vars reset, a delay slot's context imposed, and each
//! `inst_next` re-derived from this decode.

use super::RegionBuilder;
use crate::Result;
use crate::builder::{FlowCommit, hold_var};
use crate::types::{MachineInsnAddr, PcodeInsnAddr};
use anyhow::{anyhow, bail};

/// Advances one pcode index, rolling over to `insn_index = 0` of the next
/// machine instruction at the end of `lift_res`.
pub(super) fn next_pcode_addr(
    addr: PcodeInsnAddr,
    lift_res: &rsleigh::LiftRes,
) -> Result<PcodeInsnAddr> {
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
        .machine()
        .checked_add(lift_res.machine_insn_len as u64)
        .ok_or_else(|| anyhow!("machine-address overflow advancing past pcode addr {addr:?}"))?;
    Ok(PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: next_machine },
        insn_index: 0,
    })
}

/// Why a decode produced no instruction, carried as the `anyhow` payload the
/// triage sites read back.
#[derive(Debug, Clone, Copy)]
pub(in crate::builder) enum DecodeFailure {
    /// The reader had no bytes here, as opposed to Sleigh rejecting the bytes
    /// it did get.
    Unmapped(u64),
    /// No instruction decodes here: Sleigh rejected the bytes, or
    /// [`crate::CfgOptions::data_ranges`] marks them as data.
    NotCode { addr: u64, marked_data: bool },
    /// A seeded arm's code fell through past the function's end.
    PastEnd(PcodeInsnAddr),
}

impl DecodeFailure {
    /// The machine address the verdict names, whatever address the decode was
    /// asked to start from: a region's sequential decode fails several
    /// instructions past its own start, and the two sites that mean "at the
    /// address I asked for" compare this against theirs.
    pub(in crate::builder) fn addr(self) -> u64 {
        match self {
            Self::Unmapped(addr) | Self::NotCode { addr, .. } => addr,
            Self::PastEnd(at) => at.machine(),
        }
    }
}

impl std::fmt::Display for DecodeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unmapped(addr) => write!(f, "{addr:#x} is not mapped"),
            Self::NotCode {
                addr,
                marked_data: true,
            } => write!(f, "{addr:#x} is marked as data"),
            Self::NotCode {
                addr,
                marked_data: false,
            } => write!(f, "{addr:#x} does not decode"),
            Self::PastEnd(at) => write!(
                f,
                "a seeded arm's code falls through past the function's end at {:#x}",
                at.machine()
            ),
        }
    }
}

impl std::error::Error for DecodeFailure {}

/// The decode verdict `err` carries, `None` for any other failure.
///
/// Read at both ends of a failed decode: [`RegionBuilder::build`] seals its
/// region and seats a stub when a sequential fall-through lands on unmapped
/// bytes, and [`super::Builder::build`] does the same for a branch target.
/// Any other failure is Sleigh rejecting bytes it fully read, and the region
/// under construction is then real code whose extent is unknown; a stub seated
/// at its start would discard it.
pub(in crate::builder) fn decode_failure(err: &anyhow::Error) -> Option<DecodeFailure> {
    err.downcast_ref::<DecodeFailure>().copied()
}

impl<R: rsleigh::MemReader> RegionBuilder<'_, '_, R> {
    /// Each FLOWING [`strider_target::SleighArch::flow_commit_vars`] var's
    /// value at [`Self::last_lift`], the last address this builder decoded.
    ///
    /// That address is this build's own: [`Self::rederive_flow`] re-derived it
    /// as the predecessor's `inst_next`, or [`crate::builder::Builder::explore`] pinned it as
    /// the region's first. It is therefore what would have flowed on had
    /// Sleigh flowed to the successor at all, which is what an edge to an
    /// address no `inst_next` recorded needs in place of a snapshot: a direct
    /// edge taking [`crate::builder::Builder::enqueue`]'s fallback, and every
    /// [`crate::builder::Builder::enqueue_resolved`] arm.
    ///
    /// A [`FlowCommit::NoFlow`] var is left out: its commit covers one
    /// address, so nothing of it flows off the edge, and painting a live IT
    /// state onto the target predicates it.
    pub(super) fn flowing_commits(&self) -> Result<Vec<(&'static str, u32)>> {
        let Some(from) = self.last_lift else {
            return Ok(Vec::new());
        };
        self.builder
            .engine
            .tables
            .flow_commit
            .iter()
            .filter(|&&(_, class, _)| class == FlowCommit::Flowing)
            .map(|&(name, ..)| Ok((name, self.builder.engine.sleigh.get_context_at(from, name)?)))
            .collect()
    }

    /// Re-imposes this region's ISA mode at `addr` when a change point wrote
    /// over it. The read is free; only a genuine drift pays for the write.
    fn hold_isa_mode(&mut self, addr: u64) -> Result<()> {
        let (Some(want), Some(var)) = (self.isa_mode, self.builder.engine.arch.isa_mode_var())
        else {
            return Ok(());
        };
        hold_var(&mut self.builder.engine.sleigh, addr, var, want)?;
        Ok(())
    }

    /// Resets the `noflow` decode vars at `addr` to their defaults, so a value
    /// an earlier decode committed there does not change this one.
    ///
    /// The region's first instruction reads defaults. A sequential decode reads
    /// what the instruction before it committed: when `addr` holds anything
    /// but defaults, they are written and that instruction re-lifted, which
    /// commits its own values again.
    ///
    /// A region STARTING mid-instruction is the exception: the region before it
    /// just decoded those bytes, in the context committed AT them, and this
    /// decode only continues its p-code stream. Resetting would splice a
    /// stream from a different constructor onto the half already in the graph,
    /// which is how gcc `-march=armv4t`'s `movne lr,pc; bxne r3` loses its call
    /// (`LRset` is committed at the `bx`).
    fn hold_transients(&mut self, addr: u64) -> Result<()> {
        let defaults = &self.builder.engine.tables.transient_defaults;
        if defaults.is_empty() {
            return Ok(());
        }
        let prev = self.last_lift;
        if prev.is_none() && self.start_addr.insn_index != 0 {
            return Ok(());
        }
        // Sealed onto rather than decoded: `build` discards this lift.
        if prev.is_some()
            && self
                .builder
                .index
                .get(PcodeInsnAddr::at_machine_start(addr))
                .is_some()
        {
            return Ok(());
        }
        let mut wrote = false;
        for &(name, default) in defaults {
            wrote |= hold_var(&mut self.builder.engine.sleigh, addr, name, default)?;
        }
        if wrote && let Some(prev) = prev {
            self.lift_unheld(prev)?;
        }
        Ok(())
    }

    /// Holds the context at every instruction start inside the lift at `addr`,
    /// returning whether it wrote any.
    ///
    /// Sleigh decodes a delay slot as part of its branch but reads the context
    /// at the slot's own address, so the slot takes the branch's ISA mode and
    /// the `noflow` defaults, plus what the branch itself commits there.
    fn hold_delay_slot(&mut self, addr: u64, len: usize) -> Result<bool> {
        if !self.builder.engine.arch.has_delay_slots() {
            return Ok(false);
        }
        let isa = self.builder.engine.arch.isa_mode_var().zip(self.isa_mode);
        let defaults = &self.builder.engine.tables.transient_defaults;
        let end = addr.saturating_add(len as u64);
        let mut wrote = false;
        // Every MIPS encoding is a whole number of 16-bit words.
        for at in (addr.saturating_add(2)..end).step_by(2) {
            for (name, want) in isa.into_iter().chain(defaults.iter().copied()) {
                wrote |= hold_var(&mut self.builder.engine.sleigh, at, name, want)?;
            }
        }
        Ok(wrote)
    }

    /// Makes the committable flow context at `at` this build's own: erase each
    /// var to what THIS build puts there, then re-lift `from` so that
    /// instruction's own `globalset` lands again.
    ///
    /// Never a live snapshot of `at` itself, which is the value an earlier
    /// build may have committed. What to erase TO differs by flow class,
    /// because `set_context_at` is `ContextDatabase::setVariable` and paints
    /// FORWARD to the next change point:
    ///
    /// - [`FlowCommit::NoFlow`] erases to the pspec DEFAULT. The engine's own
    ///   `noflow` commit covers one address, so painting a live IT state
    ///   forward would predicate everything after it, and a `cond_shft`
    ///   shifted by the next instruction can land on a `cond_full` no `thfcc`
    ///   constructor covers, failing the decode outright. The default is what
    ///   an un-committed address already holds, so painting it forward changes
    ///   nothing, and a committed address beyond `at` gets its value back from
    ///   its own predecessor's decode.
    /// - [`FlowCommit::Flowing`] erases to the value in force at `from`, which
    ///   is what would have flowed to `at` had nothing been committed there.
    ///   The default is wrong here: it would cancel a mode this build's own
    ///   earlier instruction established and hold only where the committing
    ///   instruction is the immediate predecessor.
    ///
    /// Re-lifting is cheap: `Sleigh` memoises recently-parsed instructions,
    /// and `oneInstruction` applies a cached parse's commits too.
    fn rederive_flow(&mut self, at: u64, from: u64) -> Result<()> {
        let mut wrote = false;
        for &(name, flow, default) in &self.builder.engine.tables.flow_commit {
            let want = match flow {
                FlowCommit::NoFlow => default,
                FlowCommit::Flowing => self.builder.engine.sleigh.get_context_at(from, name)?,
            };
            wrote |= hold_var(&mut self.builder.engine.sleigh, at, name, want)?;
        }
        if wrote {
            self.lift_unheld(from)?;
        }
        Ok(())
    }

    /// Decodes `addr`: the region's first address, whose carried context
    /// `Builder::explore` pinned, or `expected_next`.
    fn lift_one(&mut self, addr: u64) -> Result<rsleigh::LiftRes> {
        debug_assert!(
            self.last_lift.is_none() || self.expected_next == Some(addr),
            "lift_one off the sequential path at {addr:#x}"
        );
        self.hold_transients(addr)?;
        self.hold_isa_mode(addr)?;
        let mut lift = self.lift_unheld(addr)?;
        if self.hold_delay_slot(addr, lift.machine_insn_len)? {
            // The first decode may have committed the mode at a target below,
            // painting `addr` itself.
            self.hold_isa_mode(addr)?;
            lift = self.lift_unheld(addr)?;
        }
        self.last_lift = Some(addr);
        // `inst_next` of an instruction THIS build decoded, which is what
        // `Builder::enqueue` may trust a snapshot of. Recorded HERE, not in
        // `process_new_insn`, which runs only from the per-p-code-op loop: an
        // instruction lifting to ZERO ops never reaches it, and ARM `enterx`
        // is exactly that shape while committing `TEEMode` at its own
        // `inst_next`. `checked_add` so an instruction at the top of the
        // address space cannot seed a wrapped low address.
        // `as u64` not `as u32`: `machine_insn_len` is a `usize`, and narrowing
        // it first would wrap a huge length into a small one and defeat the
        // `checked_add`. A zero length would make `next == addr`, erasing the
        // context at the address just decoded and recording an instruction's own
        // start as a commit point; `next_pcode_addr` rejects that length too, but
        // only once the region does not close first.
        let len = lift.machine_insn_len as u64;
        let next = (len > 0).then(|| addr.checked_add(len)).flatten();
        if let Some(next) = next {
            // Re-derived NOW, while `addr` is still the last instruction
            // decoded, so whatever sits at `next` afterwards is exactly what
            // THIS decode committed. That is what lets `Builder::enqueue`
            // trust a snapshot here even for a fall-through this region does
            // not go on to decode itself.
            self.rederive_flow(next, addr)?;
            self.builder.committed_next.insert(next);
        }
        self.expected_next = next;
        Ok(lift)
    }

    /// One Sleigh decode at `addr`, in whatever context the engine holds there.
    fn lift_unheld(&mut self, addr: u64) -> Result<rsleigh::LiftRes> {
        self.builder.engine.sleigh.lift_one(addr).map_err(|e| {
            // Display, not Debug: the error's `Debug` nests its source's, which
            // for a reader error carries a captured backtrace.
            let msg = format!("sleigh could not lift {addr:#x}: {e}");
            // `DataUnavailErr` is the reader answering a read with zero bytes,
            // which is the same verdict its own error is.
            // `PartiallyInitializedInsn` is the same window ending mid
            // instruction: the missing bytes were never read either.
            match e {
                rsleigh::error::GenericError::MemReadErr(_)
                | rsleigh::error::GenericError::Base(
                    rsleigh::error::BaseError::DataUnavailErr
                    | rsleigh::error::BaseError::PartiallyInitializedInsn,
                ) => anyhow::Error::new(DecodeFailure::Unmapped(addr)).context(msg),
                // Every decode Sleigh rejects (`BadDataError`, an unimplemented
                // constructor) surfaces as a C++ exception.
                rsleigh::error::GenericError::Base(
                    rsleigh::error::BaseError::UnexpectedCppException(_),
                ) => anyhow::Error::new(DecodeFailure::NotCode {
                    addr,
                    marked_data: false,
                })
                .context(msg),
                rsleigh::error::GenericError::Base(_) => anyhow!(msg),
            }
        })
    }

    /// [`Self::lift_one`], refusing an instruction any byte of which
    /// [`crate::CfgOptions::data_ranges`] marks as data, except the one at the
    /// function start: the caller named it as code.
    pub(super) fn lift_code(&mut self, addr: u64) -> Result<rsleigh::LiftRes> {
        if addr == self.builder.start_addr {
            return self.lift_one(addr);
        }
        let data = || DecodeFailure::NotCode {
            addr,
            marked_data: true,
        };
        let ranges = &self.builder.options.data_ranges;
        if ranges.overlaps(addr, addr.saturating_add(1)) {
            return Err(data().into());
        }
        let lifted = self.lift_one(addr)?;
        if ranges.overlaps(addr, addr.saturating_add(lifted.machine_insn_len as u64)) {
            return Err(data().into());
        }
        Ok(lifted)
    }
}
