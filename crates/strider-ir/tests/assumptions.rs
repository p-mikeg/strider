//! `AssumptionOptions`: the floor, the claim-by-claim union, and the claims
//! one set leaves out of another.

use strider_ir::AssumptionOptions;

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
    assert!(!a.escape_analysis);
    assert!(!a.return_slots_survive_untracked_writes);
    assert!(a.noalias_allocators.is_empty());
}

/// Each claim alone is omitted by the floor under its own name, and the
/// floor omits nothing a claim holds.
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
    let mut allocators = AssumptionOptions {
        noalias_allocators: [1].into_iter().collect(),
        ..AssumptionOptions::none()
    };
    allocators.union_with(&AssumptionOptions {
        noalias_allocators: [2].into_iter().collect(),
        ..AssumptionOptions::none()
    });
    assert_eq!(allocators.noalias_allocators, [1, 2].into_iter().collect());
}
