//! Claims about the analysed code that a lift or an optimizer run may rest on.

/// One claim's lattice. The type's `Default` is the floor (the claim not held).
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

/// A set-valued claim: the join is the union, containment is inclusion.
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

/// A claims struct from one `name: type = default` row per claim, with its floor constructor,
/// `union_with`, `omitted_by` and `Default`. The invoking module needs [`Claim`] in scope.
///
/// The floor sets each field to its type's `Default`, not the struct's, so a default-on claim
/// cannot leak into the floor.
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

            /// Adds every claim `other` holds: each flag's OR, each set's union.
            $vis fn union_with(&mut self, other: &Self) {
                $(Claim::join(&mut self.$claim, &other.$claim);)*
            }

            /// The names of the claims `self` holds that `other` does not, in declaration
            /// order. A set is named when `other` lacks one of its members.
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
    /// Assertions about the analysed code that the analysis cannot check. Each field's
    /// positive value is the risky one: turned on, it can make the answer wrong on valid
    /// input. [`AssumptionOptions::none`] is the sound floor of these fields, under the
    /// premises stated there.
    ///
    /// Five default on: [`stack_global_disjoint`](Self::stack_global_disjoint),
    /// [`assume_incoming_args_survive_calls`](Self::assume_incoming_args_survive_calls),
    /// [`return_slots_survive_untracked_writes`](Self::return_slots_survive_untracked_writes),
    /// [`msr_register_keeps_mode`](Self::msr_register_keeps_mode) and
    /// [`fp_rounding_mode_is_nearest`](Self::fp_rounding_mode_is_nearest). `strider_lift`'s
    /// `build_ir` holds none.
    ///
    /// Claims accumulate per [`Function`](crate::Function) through
    /// [`Function::assume`](crate::Function::assume) and are never withdrawn; only a fresh
    /// lift drops one.
    pub struct AssumptionOptions {
        /// The stack never overlaps a constant-address region (`.data`, `.rodata`, `.bss`,
        /// MMIO). Wrong only where a constant address equals `sp + K` at runtime. Default on.
        pub stack_global_disjoint: bool = true,
        /// A `Call` leaves the incoming stack-argument slots as it found them, so an argument
        /// read after a call is still detected as one. Decides only which loads are incoming
        /// arguments, never a memory edge. Default on.
        ///
        /// Holds for a conforming callee not handed a pointer into the incoming-argument area
        /// ([`incoming_args_start`](strider_target::BuiltCallingConvention::incoming_args_start)).
        /// Where a stack address that may point there reaches a callee (a call argument, a
        /// stored value, a return, a clobbered register at a call, or any use the analysis
        /// cannot place), every call blocks; entry SP plus a constant below the area does not
        /// count. Missed: such a base spilled and reloaded, an address in a callee-preserved
        /// register, and a memory-clobbering `CallOther`. Code that stores its entry SP (the
        /// ARM APCS frame record) blocks at every call.
        ///
        /// A custom convention that homes or splits register arguments next to the stack ones
        /// (ARM, MIPS, PPC64, riscv, Win64) must set
        /// [`reg_args_adjoin_stack_args`](strider_target::BuiltCallingConvention::reg_args_adjoin_stack_args).
        pub assume_incoming_args_survive_calls: bool = true,
        /// A `Store` rooted at an SP base other than the entry SP (e.g. `sp & -16`) is
        /// disjoint from the probed location. Read only by incoming-argument detection.
        /// Default off.
        pub distinct_sp_bases_disjoint: bool = false,
        /// A callee leaves the outgoing-argument slots the caller wrote as it found them, so a
        /// spill at the stack top survives the call. The psABIs let a callee write them.
        /// Default off.
        ///
        /// Inert without [`escape_analysis`](Self::escape_analysis) or a non-empty
        /// [`noalias_allocators`](Self::noalias_allocators). Only `LoadForward`'s forwarding
        /// honours it; where it is wrong, a dispatch value folded across a call can seat a
        /// wrong branch edge.
        pub callee_preserves_stack_args: bool = false,
        /// Callee addresses of pure `noalias` heap allocators (a size in, a fresh pointer out,
        /// no pointer arguments). An allocation is disjoint from every other live allocation,
        /// every stack slot and every constant address, and a load steps through the call.
        /// Empty by default.
        ///
        /// Listing a callee that can return an existing or interior pointer (`realloc`,
        /// `free`, an allocator over a static or caller-supplied arena) is unsound.
        /// Deallocation is not modelled: a use-after-free load is taken as disjoint from the
        /// storage handed out again. A non-empty set also forwards a stack spill across a
        /// listed call on a private frame, with the gaps
        /// [`escape_analysis`](Self::escape_analysis) documents.
        pub noalias_allocators: rustc_hash::FxHashSet<u64> = rustc_hash::FxHashSet::default(),
        /// When no stack address escapes to a callee, forward a spill `Load` across a `Call`
        /// and, under [`stack_global_disjoint`](Self::stack_global_disjoint), past an opaque
        /// (`Anchor`) store. Default off.
        ///
        /// An address escapes through a call argument, a stored value, a return, or a
        /// clobbered register holding it at a call. Two gaps remain: a callee reading an
        /// address from a register its convention preserves, and an unwritten argument slot
        /// (an alignment hole) read as the end of the outgoing-argument window, so a callee's
        /// argument slot above it can be forwarded across the call. Inert on a frame that
        /// stores its entry SP (every PowerPC frame, an ARM APCS frame record).
        pub escape_analysis: bool = false,
        /// A RAM `Store` the analysis cannot place on the entry stack, and any callee or
        /// user-op, leave the slots holding the saved return address and entry SP as they
        /// found them. Read only by return-target verification; off, such a return is
        /// reported unverified. A store the analysis does place is checked against the slot
        /// regardless. Needed for ARM `push {lr}; ... bl ...; pop {pc}`. Default on.
        pub return_slots_survive_untracked_writes: bool = true,
        /// An ARM register-form `msr` to the CPSR control field (`msr cpsr_c, rN`) writes the
        /// mode already in force, so it re-banks nothing; the masks and flags still update.
        /// The immediate form and `cps` still switch mode. Off, the write re-banks unless its
        /// mode is provably the current one.
        ///
        /// Wrong for a new mode built in a register (Linux's `safe_svcmode_maskall`, the
        /// switch into FIQ in `fiqasm.S`) and for a saved mode restored after a switch made
        /// since the save. Read by the lift only (`AnalyzeOptions::assumptions`); added to a
        /// lifted function it changes nothing. Default on.
        pub msr_register_keeps_mode: bool = true,
        /// The dynamic rounding mode a float-to-integer, narrowing or integer-to-float
        /// conversion reads is round to nearest even: x86 MXCSR.RC at the SSE and scalar
        /// VEX conversions that read it (`cvtsd2si`, `cvtss2si`, `cvtpd2dq`, `cvtps2dq`,
        /// `cvtpd2pi`, `cvtps2pi`, `cvtsd2ss`, `cvtpd2ps`, `cvtdq2ps`, `cvtpi2ps`,
        /// `cvtsi2ss`, the 64-bit `cvtsi2sd`), x87 RC at `fst` / `fstp` to a single or
        /// double, PowerPC FPSCR.RN at `fctid`, `fctiw`, `fctidu`, `fctiwu`, `fcfid`,
        /// `fcfids`, `fcfidu`, `fcfidus` and `frsp`, ARM FPSCR.RMode at `vcvtr`,
        /// `vcvt.f32.f64`, `vcvt.f32.s32`, `vcvt.f32.u32` and `vcvtb` / `vcvtt` to half,
        /// AArch64 FPCR.RMode at `fcvt` to a narrower float, `fcvtn`, `scvtf` / `ucvtf`
        /// from a general register, `scvtf` from an FP one, `bfcvt` and `bfcvtn`, and MIPS
        /// FCSR.RM at `cvt.w`, `cvt.l`, `cvt.s.d`, `cvt.s.w`, `cvt.s.l` and `cvt.d.l`.
        /// Fixed-mode forms (`cvtt*`, `fcti*z`, ARM `vcvt` to an integer, MIPS `round` /
        /// `trunc` / `floor` / `ceil`, AArch64 `fcvt{n,a,m,p,z}{s,u}` and `fcvtxn`) read no
        /// mode, and x87 `fist`, `fistp` and `frndint` always read RC.
        ///
        /// Off, each listed conversion selects among the four modes, so a mode the function
        /// sets (`ldmxcsr`, `fldcw`, `mtfsfi`, `vmsr`, `ctc1`) folds and an unknown one
        /// stays a select; an AArch64 FPCR read is volatile, so there it always stays one.
        /// Wrong after `fesetround` or under a mode a caller set. Float arithmetic and the
        /// conversions lifted as opaque user ops are outside it; the rest of the float
        /// environment is [`AssumptionOptions::none`]'s premise.
        ///
        /// Read by the lift only (`AnalyzeOptions::assumptions`); added to a lifted
        /// function it changes nothing. Default on.
        pub fp_rounding_mode_is_nearest: bool = true,
    }

    /// Every claim cleared: the sound floor of these fields.
    ///
    /// Two premises no field lifts. Memory is RAM: a load reads the last value the analysed
    /// code stored there, and no device, thread or signal handler changes it in between. The
    /// floating-point environment is the IEEE default apart from the conversion rounding mode
    /// [`fp_rounding_mode_is_nearest`](Self::fp_rounding_mode_is_nearest) claims: float
    /// arithmetic rounds to nearest, and x86 MXCSR.FTZ and DAZ, PowerPC FPSCR.NI, OE and UE,
    /// ARM FPSCR.FZ, AArch64 FPCR.FZ and MIPS FCSR.FS are clear (not so under `-ffast-math`).
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
