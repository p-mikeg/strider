//! The claims about the code under analysis a lift or an optimizer run may
//! rest on.

/// Assertions about the code being analysed, none of which the analysis can
/// check.  Every field's risky value is the positive one, and each one turned
/// on can make the answer wrong on valid input; the miscompile is then the
/// caller's.  [`AssumptionOptions::none`] is the sound floor of THESE fields
/// under any input program whose memory behaves as RAM and whose
/// floating-point controls are the IEEE default (see there);
/// strider-orchestrator's `AnalyzeOptions::resolve_indirect_branches` defaults
/// on and is outside it.
///
/// Five default ON.  Three hold for every compiler whose output this
/// analyses, on the premises their own docs state:
/// [`stack_global_disjoint`](Self::stack_global_disjoint), without
/// which no stack store is disjoint from a global load nor global store from a
/// stack load,
/// [`assume_incoming_args_survive_calls`](Self::assume_incoming_args_survive_calls),
/// without which a stack argument read after a call is not detected as one, and
/// [`return_slots_survive_untracked_writes`](Self::return_slots_survive_untracked_writes),
/// without which a return after any call is reported as unverified.  The
/// fourth, [`msr_register_keeps_mode`](Self::msr_register_keeps_mode), fails
/// only for ARM code that switches mode through a register `msr`, and without
/// it an interrupt-mask restore the analysis cannot place re-banks `sp` and
/// `lr`.  The fifth,
/// [`fp_rounding_mode_is_nearest`](Self::fp_rounding_mode_is_nearest), fails
/// only for code that changes the SSE or PowerPC rounding mode, and without
/// it every rounding conversion selects among four modes.  A
/// lift holds the claims it is given and `strider_lift`'s `build_ir` none, so
/// a test runs at the floor unless it claims one.
///
/// Claims accumulate per [`Function`](crate::Function):
/// [`Function::assume`](crate::Function::assume) merges a set into
/// [`Function::assumptions`](crate::Function::assumptions), which every pass
/// reads, and nothing withdraws one, so only a fresh lift drops one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumptionOptions {
    /// The stack and the global / constant-address regions (`.data`,
    /// `.rodata`, `.bss`, MMIO) never overlap at runtime, so the walker steps
    /// through a constant-address `Store` when looking back from an SP-rooted
    /// `Load` and vice-versa, and return-target verification passes a
    /// constant-address store over the return slot.  Wrong only where a
    /// constant address coincidentally equals `sp + K` at runtime.  No other
    /// class pair reads it.  Default ON.
    pub stack_global_disjoint: bool,
    /// A `Call` on an incoming stack-argument slot's memory chain leaves the
    /// slot as it found it, so the argument is still detectable after the
    /// call.  Reaches which loads count as incoming arguments, never the memory
    /// edges: a narrowed edge is read afterwards by walks that block at every
    /// call, so narrowing uses what a call-blocking walk proves.  It holds for
    /// a conforming callee that was not handed a pointer into the incoming
    /// arguments' memory, which starts at the first stack argument, or at the
    /// first register argument's home where the convention keeps the homes
    /// just below
    /// ([`incoming_args_start`](strider_target::BuiltCallingConvention::incoming_args_start)).
    /// The handing is checked: where a stack address that may point there
    /// reaches a callee, every call blocks, as in `h(&a); return a;`.  An
    /// address handed over as a call argument, a stored value or a return,
    /// or held in a register a call clobbers, counts unless it is the entry SP
    /// plus a constant below that memory, which is taken to stay inside its
    /// own frame object.  Any other use that exposes a stack address
    /// (arithmetic the analysis cannot place, a `Phi`) counts wherever it
    /// starts.  A stored address is not followed through its reload, so a
    /// base below that memory spilled and then offset into it is missed
    /// (GCC and LLVM rematerialise such a base).  A custom convention whose
    /// ABI homes or splits register arguments next to the stack ones (ARM,
    /// MIPS, PPC64, riscv, Win64) must set
    /// [`reg_args_adjoin_stack_args`](strider_target::BuiltCallingConvention::reg_args_adjoin_stack_args).
    /// Like
    /// [`escape_analysis`](Self::escape_analysis), the check misses a callee
    /// reading an address from a register its convention preserves.  A
    /// memory-clobbering `CallOther` is outside it entirely.
    /// Code that stores its entry SP or holds it in a clobbered register at a
    /// call (the APCS frame record of ARM `-mapcs-frame`, `mov ip, sp; push
    /// {.., ip, ..}`) hands that address over, so there every call blocks and
    /// no stack argument read after a call is found, as with the claim off.
    /// Default ON.
    pub assume_incoming_args_survive_calls: bool,
    /// A `Store` rooted at a *different* SP base than the entry SP (e.g. an
    /// alignment-masked `sp & -16` frame local) addresses a region disjoint
    /// from the probed location.  Off is may-alias, the sound answer.
    ///
    /// Scoped to incoming-argument detection: it is the only walk that reads
    /// it, so it never widens what `LoadForward` forwards.
    pub distinct_sp_bases_disjoint: bool,
    /// A callee leaves the outgoing-argument slots the caller wrote as it
    /// found them, so a spill at the stack top survives the call. The psABIs
    /// let a callee write those slots (they are its parameters' storage); this
    /// asserts compiler output does not.
    ///
    /// Not standalone: the only reader is the outgoing-argument-window test,
    /// which is reached only under
    /// [`escape_analysis`](Self::escape_analysis) or a non-empty
    /// [`noalias_allocators`](Self::noalias_allocators), both off by default.
    /// Set alone it changes nothing.
    ///
    /// Honoured by `LoadForward`'s forwarding alone.  Stack-argument
    /// collection, incoming-argument detection, the jump-table walk and
    /// `LoadForward`'s narrowing pin it off, keeping the full window: each
    /// decides something a later reader takes as settled (a call's arguments,
    /// the function's, a CFG edge, a memory edge the stricter walks then read),
    /// where a wrong narrowing is not a lost fold but a wrong answer.  The
    /// branch classifiers still read values forwarded under it, so where it
    /// is wrong a dispatch value folded across a call seats a wrong edge.
    pub callee_preserves_stack_args: bool,
    /// Callee addresses of pure `noalias` heap allocators (`malloc`/`calloc`-like:
    /// a size in, a fresh non-overlapping pointer out, no pointer arguments).
    /// Distinct allocations are disjoint and a load steps through such a call.
    /// An allocation also overlaps no stack slot and no constant address (a
    /// global, `.rodata`, MMIO), so an allocator carving from a static or a
    /// caller-supplied arena does not qualify.
    /// Empty (the default) leaves the feature off.
    ///
    /// Each address must genuinely satisfy that contract.  Listing one that can
    /// return an existing or interior pointer (`realloc`, `free`, an
    /// aligned-alloc taking a pointer) produces unsound Disjoint verdicts.
    ///
    /// Disjointness holds among allocations live at once.  Nothing models
    /// deallocation, so a freed allocation stays a distinct base and a stale
    /// pointer is taken as disjoint from the storage the allocator handed out
    /// again.  The load that exposes this is a use-after-free.
    ///
    /// A non-empty set also forwards a stack spill across a listed call when the
    /// frame is provably private, so it carries the gaps
    /// [`escape_analysis`](Self::escape_analysis) documents, without that knob
    /// being set.
    ///
    /// `LoadForward`'s narrowing reads the distinct allocation but steps
    /// through no listed call, incoming-argument detection's narrowing reads a
    /// heap base as a may-alias opaque pointer, and return-target verification
    /// passes a store to a listed allocation over the return slot.
    pub noalias_allocators: rustc_hash::FxHashSet<u64>,
    /// When the function's frame is provably private (no stack address escapes
    /// to any callee), forward a spill `Load` across a `Call` and step it past
    /// an opaque (`Anchor`) store. Both rest on the same axiom: nothing outside
    /// the frame can name a private slot, barring a fabricated pointer.
    ///
    /// A stack address escapes through a call argument, a stored value, a
    /// return, or a register a call clobbers holding it at the call, which
    /// covers a callee reading a register its convention does not pass (i386
    /// `regparm`, clang `fastcc`, the GNU C static chain, AArch64's X8).  It
    /// sits here because the proof keeps two gaps the IR cannot close: a callee
    /// reading an address from a register its convention preserves, and an
    /// argument slot the caller leaves unwritten (an alignment hole, or an
    /// argument it never stores) read as the end of the outgoing-argument
    /// window, so a callee's own argument slot above it can be forwarded
    /// across the call.
    pub escape_analysis: bool,
    /// A RAM `Store` whose address this analysis cannot place on the entry
    /// stack, and any callee or user-op, leave the stack slots the function
    /// saved its return address and its entry stack pointer in as it found
    /// them.  Off is the sound answer: a return whose slot an unplaced store
    /// or a call could reach is then reported as unverified.
    ///
    /// Read by return-target verification alone, where it covers code that
    /// rewrites its own return slot through a pointer no SP decomposition
    /// reaches, and code that hands the slot's address to a callee.  A store
    /// the address spine DOES place is checked against the slot whatever this
    /// says; a guessed location is not a placement, so clearing this is what
    /// brings one in.  A constant-address store under
    /// [`stack_global_disjoint`](Self::stack_global_disjoint), and a store
    /// through a [`noalias_allocators`](Self::noalias_allocators) result,
    /// pass whatever this says.  Default ON.
    ///
    /// The shape that needs it is an ARM `push {lr}; ... bl ...; pop {pc}`
    /// return: the call in between writes memory the walk cannot place, and
    /// only this claim carries the walk back to the `lr` spill.
    pub return_slots_survive_untracked_writes: bool,
    /// The mode an ARM register-form `msr` writes to the CPSR control field
    /// (`msr cpsr_c, rN`) is the mode already in force, so the write changes
    /// neither the mode nor the banked registers (`r8`-`r12` in and out of
    /// FIQ, `sp`, `lr`, the SPSR); the masks and flags it writes still
    /// update.  The immediate form and `cps` still switch mode.  Off, the
    /// write re-banks unless its mode is provably the current one, which it is
    /// not where the saved flags merge from two saves, come from memory or
    /// arrive in an argument.
    ///
    /// Wrong in two cases, where the switch is missed and the banked
    /// registers after it read as the old mode's: a new mode built in a
    /// register (Linux's boot-time `safe_svcmode_maskall`, the switch into FIQ
    /// in `fiqasm.S`), and a saved mode restored after a switch made since the
    /// save, by `cps` or either form of `msr` (the return to SVC in
    /// `fiqasm.S`'s `__set_fiq_regs`).
    ///
    /// Read by the lifter, so it counts only when the lift is given it
    /// (`AnalyzeOptions::assumptions`); added to a lifted function it changes
    /// nothing.  Default ON.
    pub msr_register_keeps_mode: bool,
    /// The dynamic rounding mode a float-to-integer or narrowing conversion
    /// rounds by is round to nearest even: x86 MXCSR.RC (bits 13-14) at every
    /// SSE and VEX conversion that reads it (`cvtsd2si`, `cvtss2si`,
    /// `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`, `cvtps2pi`, `cvtsd2ss`,
    /// `cvtpd2ps`, `cvtdq2ps`, `cvtpi2ps`, `cvtsi2ss`, the 64-bit `cvtsi2sd`
    /// and the scalar VEX forms), PowerPC FPSCR.RN at `fctid`, `fctiw`,
    /// `fctidu`, `fctiwu`, `fcfid`, `fcfids`, `fcfidu`, `fcfidus`, `frsp`
    /// and `xscvdpspn`, ARM FPSCR.RMode (bits 22-23) at `vcvtr`, and MIPS
    /// FCSR.RM (bits 0-1) at `cvt.w` and `cvt.l`.  The forms whose rounding
    /// is fixed (`cvtt*`, `fcti*z`, ARM `vcvt`, MIPS `round`, `trunc`,
    /// `floor` and `ceil`, and every AArch64 `fcvt*`) read no mode, and the
    /// x87 stores read the control word whatever this says.  Off, each
    /// conversion reads the mode and selects among its four, so a mode the
    /// function sets (`ldmxcsr`, `mtfsfi`, `vmsr`, `ctc1`) folds and an
    /// unknown one stays a select.
    ///
    /// Wrong where the code runs under another mode: after `fesetround`, or
    /// a mode a caller set or this analysis cannot place.  Outside it: the
    /// float arithmetic, taken to round to nearest either way, and the
    /// conversions lifted as opaque user ops (the packed VEX and EVEX forms,
    /// `roundss` and its kin), whose results nothing folds.  MXCSR.FTZ and
    /// DAZ are taken as 0 either way, so under `-ffast-math`, which sets them
    /// at startup, even [`AssumptionOptions::none`] narrows a denormal wrongly;
    /// so are PowerPC FPSCR.NI, OE and UE, ARM FPSCR.FZ, AArch64 FPCR.FZ and
    /// MIPS FCSR.FS, so code that sets them (non-IEEE mode, flush to zero,
    /// enabled overflow or underflow traps) has no floor here either.
    ///
    /// Read by the lifter, so it counts only when the lift is given it
    /// (`AnalyzeOptions::assumptions`); added to a lifted function it changes
    /// nothing.  Default ON.
    pub fp_rounding_mode_is_nearest: bool,
}

impl AssumptionOptions {
    /// Every claim cleared: forwards solely what the IR structurally proves,
    /// the sound floor of these fields under any input program.
    ///
    /// The proof still takes memory to be RAM: a load reads back the last value the
    /// analysed code stored at its address, and nothing outside that code (a
    /// device, another thread, a signal handler) changes it in between.  A
    /// memory-mapped register polled after a write reads as the value written.
    /// It also takes the floating-point controls other than the rounding mode
    /// to be the IEEE default: float arithmetic rounds to nearest, and x86
    /// MXCSR.FTZ and DAZ, PowerPC FPSCR.NI, OE and UE, ARM FPSCR.FZ, AArch64
    /// FPCR.FZ and MIPS FCSR.FS are clear, so `-ffast-math` code, whose
    /// startup sets FTZ and DAZ, has no floor here.  No field lifts either
    /// premise.
    #[must_use]
    pub fn none() -> Self {
        // Spelled out, not `..Self::default()`: a field added default-on
        // would otherwise survive `none` and break the claim above.
        Self {
            stack_global_disjoint: false,
            assume_incoming_args_survive_calls: false,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            noalias_allocators: rustc_hash::FxHashSet::default(),
            escape_analysis: false,
            return_slots_survive_untracked_writes: false,
            msr_register_keeps_mode: false,
            fp_rounding_mode_is_nearest: false,
        }
    }

    /// Adds every claim `other` holds: the claim-by-claim OR, and the union of
    /// the allocator sets.  Every claim's positive value is the risky one, so
    /// this is the join two runs' claims meet at.
    pub fn union_with(&mut self, other: &Self) {
        let Self {
            stack_global_disjoint,
            assume_incoming_args_survive_calls,
            distinct_sp_bases_disjoint,
            callee_preserves_stack_args,
            noalias_allocators,
            escape_analysis,
            return_slots_survive_untracked_writes,
            msr_register_keeps_mode,
            fp_rounding_mode_is_nearest,
        } = other;
        self.stack_global_disjoint |= *stack_global_disjoint;
        self.assume_incoming_args_survive_calls |= *assume_incoming_args_survive_calls;
        self.distinct_sp_bases_disjoint |= *distinct_sp_bases_disjoint;
        self.callee_preserves_stack_args |= *callee_preserves_stack_args;
        self.noalias_allocators.extend(noalias_allocators);
        self.escape_analysis |= *escape_analysis;
        self.return_slots_survive_untracked_writes |= *return_slots_survive_untracked_writes;
        self.msr_register_keeps_mode |= *msr_register_keeps_mode;
        self.fp_rounding_mode_is_nearest |= *fp_rounding_mode_is_nearest;
    }

    /// The claims `self` holds that `other` does not, by field name in
    /// declaration order.  `noalias_allocators` is named when `other` lacks
    /// an address `self` lists.
    #[must_use]
    pub fn omitted_by(&self, other: &Self) -> Vec<&'static str> {
        let Self {
            stack_global_disjoint,
            assume_incoming_args_survive_calls,
            distinct_sp_bases_disjoint,
            callee_preserves_stack_args,
            noalias_allocators,
            escape_analysis,
            return_slots_survive_untracked_writes,
            msr_register_keeps_mode,
            fp_rounding_mode_is_nearest,
        } = self;
        [
            (
                "stack_global_disjoint",
                *stack_global_disjoint && !other.stack_global_disjoint,
            ),
            (
                "assume_incoming_args_survive_calls",
                *assume_incoming_args_survive_calls && !other.assume_incoming_args_survive_calls,
            ),
            (
                "distinct_sp_bases_disjoint",
                *distinct_sp_bases_disjoint && !other.distinct_sp_bases_disjoint,
            ),
            (
                "callee_preserves_stack_args",
                *callee_preserves_stack_args && !other.callee_preserves_stack_args,
            ),
            (
                "noalias_allocators",
                !noalias_allocators.is_subset(&other.noalias_allocators),
            ),
            (
                "escape_analysis",
                *escape_analysis && !other.escape_analysis,
            ),
            (
                "return_slots_survive_untracked_writes",
                *return_slots_survive_untracked_writes
                    && !other.return_slots_survive_untracked_writes,
            ),
            (
                "msr_register_keeps_mode",
                *msr_register_keeps_mode && !other.msr_register_keeps_mode,
            ),
            (
                "fp_rounding_mode_is_nearest",
                *fp_rounding_mode_is_nearest && !other.fp_rounding_mode_is_nearest,
            ),
        ]
        .into_iter()
        .filter_map(|(name, omitted)| omitted.then_some(name))
        .collect()
    }

    /// Whether `self` holds every claim `other` holds: `other.omitted_by(self)`
    /// is empty.
    #[must_use]
    pub fn holds_all(&self, other: &Self) -> bool {
        let Self {
            stack_global_disjoint,
            assume_incoming_args_survive_calls,
            distinct_sp_bases_disjoint,
            callee_preserves_stack_args,
            noalias_allocators,
            escape_analysis,
            return_slots_survive_untracked_writes,
            msr_register_keeps_mode,
            fp_rounding_mode_is_nearest,
        } = other;
        (!stack_global_disjoint || self.stack_global_disjoint)
            && (!assume_incoming_args_survive_calls || self.assume_incoming_args_survive_calls)
            && (!distinct_sp_bases_disjoint || self.distinct_sp_bases_disjoint)
            && (!callee_preserves_stack_args || self.callee_preserves_stack_args)
            && noalias_allocators.is_subset(&self.noalias_allocators)
            && (!escape_analysis || self.escape_analysis)
            && (!return_slots_survive_untracked_writes
                || self.return_slots_survive_untracked_writes)
            && (!msr_register_keeps_mode || self.msr_register_keeps_mode)
            && (!fp_rounding_mode_is_nearest || self.fp_rounding_mode_is_nearest)
    }
}

impl Default for AssumptionOptions {
    fn default() -> Self {
        Self {
            stack_global_disjoint: true,
            assume_incoming_args_survive_calls: true,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            noalias_allocators: rustc_hash::FxHashSet::default(),
            escape_analysis: false,
            return_slots_survive_untracked_writes: true,
            msr_register_keeps_mode: true,
            fp_rounding_mode_is_nearest: true,
        }
    }
}
