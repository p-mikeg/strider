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
    let pcode_count = lift_res.insns.len() as u64;
    if addr.insn_index + 1 < pcode_count {
        return Ok(PcodeInsnAddr {
            machine_addr: addr.machine_addr,
            insn_index: addr.insn_index + 1,
        });
    }
    // A zero length would re-lift the same address forever.
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

/// Why a decode produced no instruction, carried as the `anyhow` payload.
#[derive(Debug, Clone, Copy)]
pub(in crate::builder) enum DecodeFailure {
    /// The reader had no bytes here.
    Unmapped(u64),
    /// No instruction decodes here: Sleigh rejected the bytes, or
    /// [`crate::CfgOptions::data_ranges`] marks them as data.
    NotCode { addr: u64, marked_data: bool },
    /// A seeded arm's code fell through past the function's end.
    PastEnd(PcodeInsnAddr),
}

impl DecodeFailure {
    /// The machine address the verdict names, which can lie past the address
    /// the decode started from.
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
pub(in crate::builder) fn decode_failure(err: &anyhow::Error) -> Option<DecodeFailure> {
    err.downcast_ref::<DecodeFailure>().copied()
}

impl<R: rsleigh::MemReader> RegionBuilder<'_, '_, R> {
    /// Each [`FlowCommit::Flowing`] var's value at [`Self::last_lift`], a
    /// context this build set: what an edge to an address with no recorded
    /// `inst_next` carries. `NoFlow` vars commit one address and never flow.
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

    /// Re-imposes this region's ISA mode at `addr` if a change point wrote
    /// over it.
    fn hold_isa_mode(&mut self, addr: u64) -> Result<()> {
        let (Some(want), Some(var)) = (self.isa_mode, self.builder.engine.arch.isa_mode_var())
        else {
            return Ok(());
        };
        hold_var(&mut self.builder.engine.sleigh, addr, var, want)?;
        Ok(())
    }

    /// Resets the `noflow` vars at `addr` to their defaults, re-lifting the
    /// previous instruction so its own commits land again.
    ///
    /// Not for a region starting mid-instruction: it continues a p-code
    /// stream decoded in the context committed at those bytes, and a reset
    /// would splice in another constructor's (armv4t `movne lr,pc; bxne r3`
    /// loses its call, `LRset` being committed at the `bx`).
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

    /// Imposes the region's ISA mode and the `noflow` defaults at every
    /// delay-slot address inside the lift at `addr`, which Sleigh decodes in
    /// the context at the slot's own address. Returns whether it wrote any.
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

    /// Makes the commit-able context at `at` this build's own: erases each var,
    /// then re-lifts `from` so its own `globalset` lands again. The erase
    /// paints forward to the next change point, so its value is per class:
    ///
    /// - [`FlowCommit::NoFlow`]: the pspec default. Its commit covers one
    ///   address; painting a live IT state forward predicates what follows
    ///   and can fail the decode.
    /// - [`FlowCommit::Flowing`]: the value in force at `from`, what would
    ///   have flowed to `at`. The default would cancel a mode an earlier
    ///   instruction set.
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
        // Here, not in `process_new_insn`: a zero-op instruction (ARM
        // `enterx`) can commit at its `inst_next` too.
        let len = lift.machine_insn_len as u64;
        let next = (len > 0).then(|| addr.checked_add(len)).flatten();
        if let Some(next) = next {
            // Now, so `next` holds exactly what this decode committed.
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
            // Both mean the reader had no (or not all) bytes.
            match e {
                rsleigh::error::GenericError::MemReadErr(_)
                | rsleigh::error::GenericError::Base(
                    rsleigh::error::BaseError::DataUnavailErr
                    | rsleigh::error::BaseError::PartiallyInitializedInsn,
                ) => anyhow::Error::new(DecodeFailure::Unmapped(addr)).context(msg),
                // How Sleigh rejects bytes (`BadDataError`, no constructor).
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

    /// [`Self::lift_one`], refusing an instruction overlapping
    /// [`crate::CfgOptions::data_ranges`], except at the function start.
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
