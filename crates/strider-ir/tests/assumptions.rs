//! `AssumptionOptions`: the floor, the defaults, the claim-by-claim union, and
//! the claims one set leaves out of another.
//!
//! Hand-written against the public surface, field by field: the point is to
//! pin what the four methods do independently of how they are generated, so a
//! field the generator forgets reddens here.

use strider_ir::AssumptionOptions;

/// Every claim at once, written out with no `..`: a field added to the
/// struct stops this compiling, which is what keeps [`each_claim`] the whole
/// list rather than most of it.
fn every_claim() -> AssumptionOptions {
    AssumptionOptions {
        stack_global_disjoint: true,
        assume_incoming_args_survive_calls: true,
        distinct_sp_bases_disjoint: true,
        callee_preserves_stack_args: true,
        noalias_allocators: [0x1000].into_iter().collect(),
        escape_analysis: true,
        return_slots_survive_untracked_writes: true,
        msr_register_keeps_mode: true,
        fp_rounding_mode_is_nearest: true,
    }
}

/// Every claim on its own, by field name, in declaration order.
fn each_claim() -> Vec<(&'static str, AssumptionOptions)> {
    let none = AssumptionOptions::none;
    vec![
        (
            "stack_global_disjoint",
            AssumptionOptions {
                stack_global_disjoint: true,
                ..none()
            },
        ),
        (
            "assume_incoming_args_survive_calls",
            AssumptionOptions {
                assume_incoming_args_survive_calls: true,
                ..none()
            },
        ),
        (
            "distinct_sp_bases_disjoint",
            AssumptionOptions {
                distinct_sp_bases_disjoint: true,
                ..none()
            },
        ),
        (
            "callee_preserves_stack_args",
            AssumptionOptions {
                callee_preserves_stack_args: true,
                ..none()
            },
        ),
        (
            "noalias_allocators",
            AssumptionOptions {
                noalias_allocators: [0x1000].into_iter().collect(),
                ..none()
            },
        ),
        (
            "escape_analysis",
            AssumptionOptions {
                escape_analysis: true,
                ..none()
            },
        ),
        (
            "return_slots_survive_untracked_writes",
            AssumptionOptions {
                return_slots_survive_untracked_writes: true,
                ..none()
            },
        ),
        (
            "msr_register_keeps_mode",
            AssumptionOptions {
                msr_register_keeps_mode: true,
                ..none()
            },
        ),
        (
            "fp_rounding_mode_is_nearest",
            AssumptionOptions {
                fp_rounding_mode_is_nearest: true,
                ..none()
            },
        ),
    ]
}

/// The default-on claims are what a caller reaching for the sound floor has
/// to clear, so `none` clearing everything is the contract.
#[test]
fn none_clears_every_claim() {
    let a = AssumptionOptions::none();
    assert!(!a.stack_global_disjoint);
    assert!(!a.assume_incoming_args_survive_calls);
    assert!(!a.distinct_sp_bases_disjoint);
    assert!(!a.callee_preserves_stack_args);
    assert!(a.noalias_allocators.is_empty());
    assert!(!a.escape_analysis);
    assert!(!a.return_slots_survive_untracked_writes);
    assert!(!a.msr_register_keeps_mode);
    assert!(!a.fp_rounding_mode_is_nearest);
}

/// Each claim alone is omitted by the floor under its own name, and a claim
/// omits nothing the floor holds.
#[test]
fn omitted_by_names_each_claim() {
    let floor = AssumptionOptions::none();
    for (name, claim) in each_claim() {
        assert_eq!(claim.omitted_by(&floor), vec![name], "{name} alone");
        assert!(
            floor.omitted_by(&claim).is_empty(),
            "the floor under {name}"
        );
        assert!(claim.omitted_by(&claim).is_empty(), "{name} under itself");
    }
}

/// Every field is reported, under its own name, in declaration order: the
/// names a `Function` header and `optimize`'s refusal print.
#[test]
fn omitted_by_names_every_field_in_declaration_order() {
    let mut all = AssumptionOptions::none();
    for (_, claim) in each_claim() {
        all.union_with(&claim);
    }
    assert_eq!(
        all,
        every_claim(),
        "`each_claim` is one row per field and no more"
    );
    assert_eq!(
        all.omitted_by(&AssumptionOptions::none()),
        vec![
            "stack_global_disjoint",
            "assume_incoming_args_survive_calls",
            "distinct_sp_bases_disjoint",
            "callee_preserves_stack_args",
            "noalias_allocators",
            "escape_analysis",
            "return_slots_survive_untracked_writes",
            "msr_register_keeps_mode",
            "fp_rounding_mode_is_nearest",
        ]
    );
}

/// An allocator set is omitted when one of its addresses is missing, not
/// when the other set lists more.
#[test]
fn omitted_by_compares_allocator_sets_by_inclusion() {
    let listing = |addrs: &[u64]| AssumptionOptions {
        noalias_allocators: addrs.iter().copied().collect(),
        ..AssumptionOptions::none()
    };
    assert_eq!(
        listing(&[1, 2]).omitted_by(&listing(&[1])),
        vec!["noalias_allocators"]
    );
    assert!(listing(&[1]).omitted_by(&listing(&[1, 2])).is_empty());
    assert_eq!(
        listing(&[1]).omitted_by(&listing(&[2])),
        vec!["noalias_allocators"]
    );
}

/// No claim holds another; each holds itself and the floor, and the floor
/// holds none.
#[test]
fn omitted_by_checks_each_claim() {
    let floor = AssumptionOptions::none();
    let claims = each_claim();
    for (name, claim) in &claims {
        assert!(
            !claim.omitted_by(&floor).is_empty(),
            "the floor over {name}"
        );
        assert!(floor.omitted_by(claim).is_empty(), "{name} over the floor");
        for (other_name, other) in &claims {
            assert_eq!(
                other.omitted_by(claim).is_empty(),
                name == other_name,
                "{name} over {other_name}"
            );
        }
    }
}

/// The union of two claims holds both and nothing else.
#[test]
fn union_is_the_claimwise_or() {
    let claims = each_claim();
    for (left_name, left) in &claims {
        for (right_name, right) in &claims {
            let mut u = left.clone();
            u.union_with(right);
            assert!(left.omitted_by(&u).is_empty(), "{left_name} | {right_name}");
            assert!(
                right.omitted_by(&u).is_empty(),
                "{left_name} | {right_name}"
            );
            let extra = if left_name == right_name {
                vec![]
            } else {
                vec![*right_name]
            };
            assert_eq!(u.omitted_by(left), extra, "{left_name} | {right_name}");
        }
    }
}

/// Folding every claim in never drops one already in, the floor is the
/// identity, and a set unioned with itself is unchanged.
#[test]
fn union_is_monotone_and_idempotent() {
    let mut acc = AssumptionOptions::none();
    let claims = each_claim();
    for (i, (_, claim)) in claims.iter().enumerate() {
        acc.union_with(claim);
        for (name, earlier) in &claims[..=i] {
            assert!(earlier.omitted_by(&acc).is_empty(), "{name} kept");
        }
    }
    let every: Vec<&str> = claims.iter().map(|(name, _)| *name).collect();
    assert_eq!(acc.omitted_by(&AssumptionOptions::none()), every);
    let before = acc.clone();
    acc.union_with(&AssumptionOptions::none());
    assert_eq!(acc, before, "the floor adds nothing");
    let same = acc.clone();
    acc.union_with(&same);
    assert_eq!(acc, before, "a set adds nothing to itself");
}

/// A set field joins by union, not by replacement: each side's addresses
/// survive, and a repeat adds nothing.
#[test]
fn union_unions_the_allocator_set() {
    let listing = |addrs: &[u64]| AssumptionOptions {
        noalias_allocators: addrs.iter().copied().collect(),
        ..AssumptionOptions::none()
    };
    let mut allocators = listing(&[1]);
    allocators.union_with(&listing(&[2]));
    assert_eq!(allocators.noalias_allocators, [1, 2].into_iter().collect());
    allocators.union_with(&listing(&[2, 3]));
    assert_eq!(
        allocators.noalias_allocators,
        [1, 2, 3].into_iter().collect()
    );
    allocators.union_with(&AssumptionOptions::none());
    assert_eq!(
        allocators.noalias_allocators,
        [1, 2, 3].into_iter().collect(),
        "the floor withdraws no address"
    );
}

/// The defaults: exactly the five documented claims on, which are not the
/// floor.  `distinct_sp_bases_disjoint`, `callee_preserves_stack_args` and
/// `escape_analysis` are off and `noalias_allocators` is empty.
#[test]
fn default_holds_exactly_the_five_default_claims() {
    let d = AssumptionOptions::default();
    assert!(d.stack_global_disjoint);
    assert!(d.assume_incoming_args_survive_calls);
    assert!(!d.distinct_sp_bases_disjoint);
    assert!(!d.callee_preserves_stack_args);
    assert!(d.noalias_allocators.is_empty());
    assert!(!d.escape_analysis);
    assert!(d.return_slots_survive_untracked_writes);
    assert!(d.msr_register_keeps_mode);
    assert!(d.fp_rounding_mode_is_nearest);
    assert_eq!(
        d.omitted_by(&AssumptionOptions::none()),
        vec![
            "stack_global_disjoint",
            "assume_incoming_args_survive_calls",
            "return_slots_survive_untracked_writes",
            "msr_register_keeps_mode",
            "fp_rounding_mode_is_nearest",
        ]
    );
    assert!(AssumptionOptions::none().omitted_by(&d).is_empty());
}
