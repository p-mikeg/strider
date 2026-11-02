//! The claims about the code under analysis a lift or an optimizer run may
//! rest on.

/// One claim's lattice: the floor, the join, and containment.  The field's
/// type carries all three, its `Default` being the floor (the claim not
/// held), so a set-valued claim is declared in `claims!` exactly like a
/// `bool` one and no method special-cases it.
trait Claim: Default {
    /// Adds whatever `other` claims.
    fn join(&mut self, other: &Self);
    /// Whether `self` claims everything `other` does.
    fn holds(&self, other: &Self) -> bool;
}

impl Claim for bool {
    fn join(&mut self, other: &Self) {
        *self |= *other;
    }

    fn holds(&self, other: &Self) -> bool {
        *self || !*other
    }
}

/// A set-valued claim is held member by member: the join is the union and
/// containment is inclusion.
impl<T: Clone + Eq + std::hash::Hash, S: std::hash::BuildHasher + Default> Claim
    for std::collections::HashSet<T, S>
{
    fn join(&mut self, other: &Self) {
        self.extend(other.iter().cloned());
    }

    fn holds(&self, other: &Self) -> bool {
        other.is_subset(self)
    }
}

/// A claims type from one row per claim: `name: type = default`, with the
/// row's own doc comment, and the doc the floor constructor is to carry.
/// Generates the struct, the floor, the join, the claims one set leaves out
/// of another and the defaults.  The invoking module needs [`Claim`] in
/// scope; a row's type decides the three behaviours, so a second set-valued
/// claim is one more row and nothing else.
///
/// The list is written once, so no method can be given a claim it then
/// leaves out, which is what the destructuring `let Self { .. } = other;` in
/// each hand-written method used to buy.  The floor sets every field to its
/// TYPE's `Default`, never the struct's, because a claim added default-on
/// would survive that and quietly stop the floor being the sound one.
macro_rules! claims {
    (
        $(#[$struct_doc:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$claim_doc:meta])*
                $claim_vis:vis $claim:ident: $ty:ty = $default:expr
            ),* $(,)?
        }

        $(#[$floor_doc:meta])*
        fn $floor:ident();
    ) => {
        $(#[$struct_doc])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        $vis struct $name {
            $(
                $(#[$claim_doc])*
                $claim_vis $claim: $ty,
            )*
        }

        impl $name {
            $(#[$floor_doc])*
            #[must_use]
            $vis fn $floor() -> Self {
                Self { $($claim: Default::default()),* }
            }

            /// Adds every claim `other` holds: the claim-by-claim join, each
            /// flag's OR and each set's union.  Every claim's positive value
            /// is the risky one, so this is the join two runs' claims meet
            /// at.
            $vis fn union_with(&mut self, other: &Self) {
                $(Claim::join(&mut self.$claim, &other.$claim);)*
            }

            /// The claims `self` holds that `other` does not, by field name
            /// in declaration order.  A set-valued claim is named when
            /// `other` lacks a member `self` lists.
            #[must_use]
            $vis fn omitted_by(&self, other: &Self) -> Vec<&'static str> {
                [$((
                    stringify!($claim),
                    !Claim::holds(&other.$claim, &self.$claim),
                )),*]
                .into_iter()
                .filter_map(|(name, omitted)| omitted.then_some(name))
                .collect()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $($claim: $default),* }
            }
        }
    };
}

claims! {
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
    /// without which a return after a user-op or a call that does not preserve
    /// memory is reported as unverified.  The
    /// fourth, [`msr_register_keeps_mode`](Self::msr_register_keeps_mode), fails
    /// only for ARM code that switches mode through a register `msr`, and without
    /// it an interrupt-mask restore the analysis cannot place re-banks `sp` and
    /// `lr`.  The fifth,
    /// [`fp_rounding_mode_is_nearest`](Self::fp_rounding_mode_is_nearest), fails
    /// only for code that changes the SSE, x87, PowerPC, ARM VFP, AArch64 or MIPS
    /// FPU rounding mode, and without it every rounding conversion selects among
    /// four modes.  A
    /// lift holds the claims it is given and `strider_lift`'s `build_ir` none, so
    /// a test runs at the floor unless it claims one.
    ///
    /// Claims accumulate per [`Function`](crate::Function):
    /// [`Function::assume`](crate::Function::assume) merges a set into
    /// [`Function::assumptions`](crate::Function::assumptions), which every pass
    /// reads, and nothing withdraws one, so only a fresh lift drops one.
    pub struct AssumptionOptions {
        /// The stack and the global / constant-address regions (`.data`,
        /// `.rodata`, `.bss`, MMIO) never overlap at runtime, so the walker steps
        /// through a constant-address `Store` when looking back from an SP-rooted
        /// `Load` and vice-versa, and return-target verification passes a
        /// constant-address store over the return slot.  Wrong only where a
        /// constant address coincidentally equals `sp + K` at runtime.  No other
        /// class pair reads it.  Default ON.
        pub stack_global_disjoint: bool = true,
        /// A `Call` on an incoming stack-argument slot's memory chain leaves the
        /// slot as it found it, so the argument is still detectable after the
        /// call.  Reaches which loads count as incoming arguments, never the memory
        /// edges: a narrowed edge is read afterwards by walks that honour no call
        /// relaxation, so the narrowing steps through no call a claim makes
        /// transparent.  It holds for
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
        pub assume_incoming_args_survive_calls: bool = true,
        /// A `Store` rooted at a *different* SP base than the entry SP (e.g. an
        /// alignment-masked `sp & -16` frame local) addresses a region disjoint
        /// from the probed location.  Off is may-alias, the sound answer.
        ///
        /// Scoped to incoming-argument detection: it is the only walk that reads
        /// it, so it never widens what `LoadForward` forwards.
        pub distinct_sp_bases_disjoint: bool = false,
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
        pub callee_preserves_stack_args: bool = false,
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
        /// being set, and is likewise inert on a frame that stores its entry SP.
        ///
        /// The narrowing of `LoadForward`, incoming-argument detection and
        /// `CallStackArgCollect` read the distinct allocation, but none of them
        /// steps through a listed call on its account: incoming-argument
        /// detection passes one only under
        /// [`assume_incoming_args_survive_calls`](Self::assume_incoming_args_survive_calls),
        /// as it passes any call.  Return-target verification passes a store to a
        /// listed allocation over the return slot.
        pub noalias_allocators: rustc_hash::FxHashSet<u64> = rustc_hash::FxHashSet::default(),
        /// When the function's frame is provably private (no stack address escapes
        /// to any callee), forward a spill `Load` across a `Call` and, under
        /// [`stack_global_disjoint`](Self::stack_global_disjoint), step it past an
        /// opaque (`Anchor`) store. Both rest on the same axiom: nothing outside
        /// the frame can name a private slot, barring a fabricated pointer.
        /// Without that claim the opaque pointer may be a global the stack
        /// overlaps.
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
        ///
        /// A frame that stores its entry SP escapes, so the forwarding is inert
        /// on every PowerPC frame (the `stwu`/`stdu` back chain) and on an ARM
        /// APCS frame record.
        pub escape_analysis: bool = false,
        /// A RAM `Store` whose address this analysis cannot place on the entry
        /// stack, and any callee or user-op, leave the stack slots the function
        /// saved its return address and its entry stack pointer in as it found
        /// them.  Off is the sound answer: a return whose slot an unplaced store,
        /// a user-op or a call whose convention does not preserve memory could
        /// reach is then reported as unverified.
        ///
        /// Read by return-target verification alone, where it covers code that
        /// rewrites its own return slot through a pointer no SP decomposition
        /// reaches, and code that hands the slot's address to a callee.  A store
        /// the address spine DOES place is checked against the slot whatever this
        /// says; a guessed location is not a placement, so clearing this is what
        /// brings one in.  A constant-address store under
        /// [`stack_global_disjoint`](Self::stack_global_disjoint), a store
        /// through a [`noalias_allocators`](Self::noalias_allocators) result and
        /// a call whose convention preserves memory pass whatever this says.
        /// Default ON.
        ///
        /// The shape that needs it is an ARM `push {lr}; ... bl ...; pop {pc}`
        /// return: the call in between writes memory the walk cannot place, and
        /// only this claim carries the walk back to the `lr` spill.
        pub return_slots_survive_untracked_writes: bool = true,
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
        pub msr_register_keeps_mode: bool = true,
        /// The dynamic rounding mode a float-to-integer, narrowing or
        /// integer-to-float conversion rounds by is round to nearest even: x86
        /// MXCSR.RC (bits 13-14) at every SSE and VEX conversion that reads it
        /// (`cvtsd2si`, `cvtss2si`, `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`,
        /// `cvtps2pi`, `cvtsd2ss`, `cvtpd2ps`, `cvtdq2ps`, `cvtpi2ps`,
        /// `cvtsi2ss`, the 64-bit `cvtsi2sd` and the scalar VEX forms), the x87
        /// control word's RC (bits 10-11) at `fst` and `fstp` to a single or a
        /// double, PowerPC FPSCR.RN at `fctid`, `fctiw`, `fctidu`, `fctiwu`,
        /// `fcfid`, `fcfids`, `fcfidu`, `fcfidus`, `frsp` and `xscvdpspn`, ARM
        /// FPSCR.RMode (bits 22-23) at `vcvtr`, `vcvt.f32.f64`, `vcvt.f32.s32`,
        /// `vcvt.f32.u32` and `vcvtb` and `vcvtt` to half, AArch64 FPCR.RMode
        /// (bits 22-23) at `fcvt` to a narrower float, `fcvtn`, `scvtf` and
        /// `ucvtf` from a general register and `scvtf` from an FP one, and
        /// `bfcvt` and `bfcvtn`, and MIPS FCSR.RM (bits 0-1) at `cvt.w`, `cvt.l`,
        /// `cvt.s.d`, `cvt.s.w`, `cvt.s.l` and `cvt.d.l`.  The forms whose
        /// rounding is fixed (`cvtt*`, `fcti*z`, ARM `vcvt` to an integer, MIPS
        /// `round`, `trunc`, `floor` and `ceil`, and AArch64
        /// `fcvt{n,a,m,p,z}{s,u}` and `fcvtxn`) read no mode, and x87 `fist`,
        /// `fistp` and `frndint` read the control word whatever this says.  Off,
        /// each conversion reads the mode and selects among its four, so a mode
        /// the function sets (`ldmxcsr`, `fldcw`, `mtfsfi`, `vmsr`, `ctc1`) folds
        /// and an unknown one stays a select; an AArch64 FPCR read is volatile,
        /// so there even one `msr fpcr` sets stays a select.
        ///
        /// Wrong where the code runs under another mode: after `fesetround`, or
        /// a mode a caller set or this analysis cannot place.  Outside it: the
        /// float arithmetic, taken to round to nearest either way
        /// ([`AssumptionOptions::none`]), and the conversions lifted as opaque
        /// user ops (the packed VEX and EVEX forms, `roundss` and its kin, the
        /// AArch64 vector and FP-register `#fbits` forms and `ucvtf` from an FP
        /// register, the ARM NEON and
        /// fixed-point `vcvt`, `vrint*` and `frint*`), whose results nothing
        /// folds.  MXCSR.FTZ and DAZ are taken as 0 either way, so under
        /// `-ffast-math`, which sets them at startup, even
        /// [`AssumptionOptions::none`] narrows a denormal wrongly;
        /// so are PowerPC FPSCR.NI, OE and UE, ARM FPSCR.FZ, AArch64 FPCR.FZ and
        /// MIPS FCSR.FS, so code that sets them (non-IEEE mode, flush to zero,
        /// enabled overflow or underflow traps) has no floor here either.
        ///
        /// Read by the lifter, so it counts only when the lift is given it
        /// (`AnalyzeOptions::assumptions`); added to a lifted function it changes
        /// nothing.  Default ON.
        pub fp_rounding_mode_is_nearest: bool = true,
    }

    /// Every claim cleared: forwards solely what the IR structurally proves,
    /// the sound floor of these fields under any input program.
    ///
    /// The proof still takes memory to be RAM: a load reads back the last value the
    /// analysed code stored at its address, and nothing outside that code (a
    /// device, another thread, a signal handler) changes it in between.  A
    /// memory-mapped register polled after a write reads as the value written.
    /// It also takes the floating-point environment to be the IEEE default
    /// outside the conversion rounding mode
    /// [`fp_rounding_mode_is_nearest`](Self::fp_rounding_mode_is_nearest)
    /// claims: float arithmetic, scalar and vector, rounds to nearest
    /// whatever mode is in force (x87, SSE, PowerPC `fadd` and `fmadd`, ARM,
    /// AArch64 and MIPS alike), and x86 MXCSR.FTZ and DAZ, PowerPC FPSCR.NI,
    /// OE and UE, ARM FPSCR.FZ, AArch64 FPCR.FZ and MIPS FCSR.FS are clear, so `-ffast-math` code, whose startup sets
    /// FTZ and DAZ, and code computing under another mode have no floor here.
    /// No field lifts either premise.
    fn none();
}

#[cfg(test)]
mod tests {
    use super::Claim;
    use rustc_hash::FxHashSet;

    claims! {
        /// A throwaway claims type over an arbitrary field list: a claim off
        /// by default, one on, and a set.  Declared through the same macro,
        /// so what holds here is what the generator does, not what one call
        /// of it happens to produce.
        pub struct Trial {
            /// Off by default.
            pub quiet: bool = false,
            /// A set-valued claim, between the flags so the declaration
            /// order a name list reports is not the flags-then-sets one.
            pub listed: FxHashSet<u64> = FxHashSet::default(),
            /// On by default.
            pub loud: bool = true,
        }

        /// Every claim cleared, the set emptied.
        fn floor();
    }

    /// `floor` clears each field whatever its default, the set included.
    #[test]
    fn the_floor_clears_every_claim() {
        let f = Trial::floor();
        assert!(!f.quiet);
        assert!(!f.loud, "a claim default-on is still cleared");
        assert!(f.listed.is_empty());
        assert!(f.omitted_by(&Trial::default()).is_empty());
    }

    /// Each row's `= value` is what `Default` takes, nothing else.
    #[test]
    fn default_takes_each_rows_own_value() {
        let d = Trial::default();
        assert!(!d.quiet);
        assert!(d.loud);
        assert!(d.listed.is_empty());
    }

    /// A flag joins by OR, a set by union, and neither withdraws.
    #[test]
    fn union_ors_the_flags_and_unions_the_set() {
        let mut acc = Trial {
            quiet: true,
            listed: [1].into_iter().collect(),
            ..Trial::floor()
        };
        acc.union_with(&Trial {
            loud: true,
            listed: [2].into_iter().collect(),
            ..Trial::floor()
        });
        assert!(acc.quiet, "the join withdraws nothing");
        assert!(acc.loud);
        assert_eq!(acc.listed, [1, 2].into_iter().collect());
        let before = acc.clone();
        acc.union_with(&Trial::floor());
        assert_eq!(acc, before, "the floor is the identity");
        acc.union_with(&before);
        assert_eq!(acc, before, "and the join is idempotent");
    }

    /// Containment is implication on a flag and inclusion on a set.
    #[test]
    fn omitted_by_implies_on_flags_and_includes_on_sets() {
        let listing = |addrs: &[u64]| Trial {
            listed: addrs.iter().copied().collect(),
            ..Trial::floor()
        };
        let loud = Trial {
            loud: true,
            ..Trial::floor()
        };
        assert!(Trial::floor().omitted_by(&loud).is_empty());
        assert!(!loud.omitted_by(&Trial::floor()).is_empty());
        assert!(loud.omitted_by(&loud).is_empty());
        assert!(listing(&[1]).omitted_by(&listing(&[1, 2])).is_empty());
        assert!(!listing(&[1, 2]).omitted_by(&listing(&[1])).is_empty());
        assert!(!listing(&[2]).omitted_by(&listing(&[1])).is_empty());
        assert!(!listing(&[1]).omitted_by(&loud).is_empty());
    }

    /// Every field is reportable, under its own name, in declaration order,
    /// and a set is named when the other side lacks a member.
    #[test]
    fn omitted_by_names_each_field_in_declaration_order() {
        let all = Trial {
            quiet: true,
            listed: [1].into_iter().collect(),
            loud: true,
        };
        assert_eq!(
            all.omitted_by(&Trial::floor()),
            vec!["quiet", "listed", "loud"]
        );
        assert!(Trial::floor().omitted_by(&all).is_empty());
        assert!(all.omitted_by(&all).is_empty());
        let one_short = Trial {
            listed: FxHashSet::default(),
            ..all.clone()
        };
        assert_eq!(all.omitted_by(&one_short), vec!["listed"]);
        assert!(one_short.omitted_by(&all).is_empty());
    }
}
