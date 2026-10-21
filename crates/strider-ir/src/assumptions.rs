//! The claims about the code under analysis a lift or an optimizer run may
//! rest on.

/// Assertions about the code being analysed, none of which the analysis can
/// check.  Every field's risky value is the positive one, and each one turned
/// on can make the answer wrong on valid input; the miscompile is then the
/// caller's.  [`AssumptionOptions::none`] is the sound floor of THESE fields
/// under any input program whose memory behaves as RAM (see there);
/// strider-orchestrator's `AnalyzeOptions::resolve_indirect_branches` defaults
/// on and is outside it.
///
/// Four default ON.  Three hold for every compiler whose output this analyses:
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
/// `lr`.  A
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
    /// `Load` and vice-versa.  Wrong only where a constant address
    /// coincidentally equals `sp + K` at runtime.  No other class pair reads
    /// it.  Default ON.
    pub stack_global_disjoint: bool,
    /// A `Call` on an incoming stack-argument slot's memory chain leaves the
    /// slot as it found it, so the argument is still detectable after the
    /// call.  Reaches which loads count as incoming arguments, never the memory
    /// edges: a narrowed edge is read afterwards by walks that block at every
    /// call, so narrowing uses what a call-blocking walk proves.  It holds for
    /// a conforming callee that was not handed a pointer into the area, since
    /// those slots sit above the entry SP.  A memory-clobbering `CallOther` is
    /// outside it entirely.
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
    /// Passes that must hold without the claim read a heap base as a may-alias
    /// opaque pointer rather than a distinct allocation, so setting this
    /// widens what folds, never what a permanent rewrite names.
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
    /// reading an address from a register its convention preserves, and, where
    /// an argument's alignment exceeds the slot stride, the padding hole read
    /// as the end of the outgoing-argument window, so a callee's own argument
    /// slot above the hole can be forwarded across the call.
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
    /// brings one in.  Default ON.
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
}

impl AssumptionOptions {
    /// Every claim cleared: forwards solely what the IR structurally proves,
    /// the sound floor of these fields under any input program.
    ///
    /// The proof still takes memory to be RAM: a load reads back the last value the
    /// analysed code stored at its address, and nothing outside that code (a
    /// device, another thread, a signal handler) changes it in between.  A
    /// memory-mapped register polled after a write reads as the value written.
    /// No field lifts the premise.
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
        } = other;
        self.stack_global_disjoint |= *stack_global_disjoint;
        self.assume_incoming_args_survive_calls |= *assume_incoming_args_survive_calls;
        self.distinct_sp_bases_disjoint |= *distinct_sp_bases_disjoint;
        self.callee_preserves_stack_args |= *callee_preserves_stack_args;
        self.noalias_allocators.extend(noalias_allocators);
        self.escape_analysis |= *escape_analysis;
        self.return_slots_survive_untracked_writes |= *return_slots_survive_untracked_writes;
        self.msr_register_keeps_mode |= *msr_register_keeps_mode;
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
        }
    }
}
