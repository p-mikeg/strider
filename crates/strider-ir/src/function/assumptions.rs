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

/// A claims struct from `name: type = default` rows, with a floor constructor (each field
/// its type's `Default`), `union_with`, `omitted_by` and `Default`. Needs [`Claim`] in scope.
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
    /// Claims about the analysed code that the analysis cannot check. Each field's positive
    /// value is the risky one: on, it can make the answer wrong on valid input. Claims
    /// accumulate per [`Function`](crate::Function) and are never withdrawn.
    pub struct AssumptionOptions {
        /// The stack never overlaps a constant-address region (`.data`, `.rodata`, `.bss`,
        /// MMIO). Wrong where a constant address equals `sp + K` at runtime. Default on.
        pub stack_global_disjoint: bool = true,
        /// A `Call` leaves the incoming stack-argument slots as it found them. Wrong for a
        /// callee that writes them: a load after the call is misread as an argument. A
        /// convention whose register arguments adjoin the stack ones must set
        /// [`reg_args_adjoin_stack_args`](strider_target::BuiltCallingConvention::reg_args_adjoin_stack_args).
        /// Default on.
        pub assume_incoming_args_survive_calls: bool = true,
        /// A `Store` rooted at an SP base other than the entry SP (e.g. `sp & -16`) is
        /// disjoint from the probed incoming-argument slot. Wrong where the two bases meet.
        /// Default off.
        pub distinct_sp_bases_disjoint: bool = false,
        /// A callee leaves the outgoing-argument slots the caller wrote as it found them, so
        /// a spill there survives the call. Wrong for a callee that writes them, as the psABIs
        /// allow. Default off.
        pub callee_preserves_stack_args: bool = false,
        /// Callee addresses of pure `noalias` allocators: a size in, a fresh pointer out,
        /// disjoint from every other allocation, stack slot and constant address. Wrong for a
        /// callee that can return an existing or interior pointer (`realloc`, an arena).
        /// Deallocation is not modelled. Empty by default.
        pub noalias_allocators: rustc_hash::FxHashSet<u64> = rustc_hash::FxHashSet::default(),
        /// When no stack address escapes to a callee, a spill `Load` forwards across a `Call`.
        /// Wrong where a callee reaches the frame through a path escape tracking misses, such
        /// as a callee-preserved register. Default off.
        pub escape_analysis: bool = false,
        /// An unplaced RAM `Store`, callee or user-op leaves the saved return-address and
        /// entry-SP slots as it found them. Wrong where one overwrites them: the return is
        /// verified when it is not. Default on.
        pub return_slots_survive_untracked_writes: bool = true,
        /// An ARM register-form `msr` to the CPSR control field writes the mode already in
        /// force, so it re-banks nothing. Wrong for code building a new mode in a register.
        /// Lift-time only. Default on.
        pub msr_register_keeps_mode: bool = true,
        /// The dynamic rounding mode a float conversion reads is round-to-nearest-even.
        /// Wrong after `fesetround` or under a mode a caller set. Lift-time only. Default on.
        pub fp_rounding_mode_is_nearest: bool = true,
    }

    /// Every claim cleared: the sound floor, for RAM-like memory (no device, thread or signal
    /// changes it) and an IEEE-default float environment (no flush-to-zero).
    fn none();
}

#[cfg(test)]
mod tests {
    use super::Claim;
    use rustc_hash::FxHashSet;

    claims! {
        /// A claim off by default, one on, and a set.
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
