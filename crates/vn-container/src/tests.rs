use super::*;

fn reg(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: off,
        size,
    }
}
fn uniq(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        addr_space: rsleigh::VnSpace::UNIQUE,
        addr_off: off,
        size,
    }
}

#[test]
fn vn_contains_encloses_and_rejects_disjoint() {
    let rax = reg(0, 8);
    assert!(vn_contains(&rax, &reg(0, 4))); // eax contained in rax
    assert!(vn_contains(&rax, &rax)); // reflexive
    assert!(!vn_contains(&rax, &reg(16, 4))); // disjoint
    assert!(!vn_contains(&reg(0, 4), &rax)); // narrower can't enclose wider
}

#[test]
fn dedup_drops_enclosed_keeps_wider_and_partial() {
    let rdi = reg(0, 8);
    let edi = reg(0, 4);
    assert_eq!(dedup_overlapping_largest(&[rdi, edi]), vec![rdi]);
    assert_eq!(dedup_overlapping_largest(&[edi, rdi]), vec![rdi]);
    // Partial overlap: neither encloses the other, so both survive.
    assert_eq!(
        dedup_overlapping_largest(&[reg(0, 8), reg(4, 8)]),
        vec![reg(0, 8), reg(4, 8)]
    );
    // Enclosure must be STRICT: equal-size duplicates both survive.
    assert_eq!(dedup_overlapping_largest(&[reg(0, 4), reg(0, 4)]).len(), 2);
    assert!(dedup_overlapping_largest(&[]).is_empty());
}

#[test]
fn dedup_overflow_safe_on_high_offset() {
    let wide = reg(u64::MAX - 4, 8);
    let narrow = reg(u64::MAX - 4, 2);
    assert_eq!(dedup_overlapping_largest(&[wide, narrow]), vec![wide]);
}

/// Both ends run past `u64::MAX`, so a saturating `vn_end` reports them
/// equal and calls the WIDER varnode contained in the narrower, which then
/// underflows the container-shift arithmetic.
#[test]
fn vn_contains_does_not_saturate_ends_past_u64_max() {
    assert!(!vn_contains(&reg(u64::MAX - 1, 2), &reg(u64::MAX - 1, 4)));
}

#[test]
fn largest_container_resolves_subregister_and_self() {
    let rax = reg(0, 8);
    assert_eq!(largest_container_in(&[rax], &reg(0, 4)), rax);
    assert_eq!(largest_container_in(&[rax], &reg(16, 4)), reg(16, 4));
    // Non-aliasable spaces resolve to self.
    let c = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::CONST,
        addr_off: 5,
        size: 8,
    };
    assert_eq!(largest_container_in(&[], &c), c);
}

#[test]
fn container_map_picks_widest_crossing_encloser() {
    // Two enclosers of `inner` that don't enclose each other: the dropped
    // inner view must map to the WIDER one, not the first seen.
    let a = uniq(0, 12); // [0,12): encloses [5,9); crosses b; survives.
    let b = uniq(2, 18); // [2,20): encloses [5,9) and is wider; survives.
    let inner = uniq(5, 4); // [5,9): enclosed by BOTH -> dropped.

    let survivors = dedup_overlapping_largest(&[a, b, inner]);
    assert_eq!(survivors, vec![a, b]);

    let cm = ContainerMap::build(&survivors, [a, b, inner]);
    assert_eq!(cm.container_of(&inner), b, "widest encloser");
    assert_eq!(cm.container_of(&a), a);
    assert_eq!(cm.container_of(&b), b);
}

/// A read and a write of one varnode landing under different SSA
/// variables is an SSA miscompile, so the sweep and the scan must answer
/// identically over a nested register file.
#[test]
fn container_map_and_linear_scan_agree_over_a_nested_file() {
    // Three-level nesting plus a crossing pair, the shapes a real
    // register file mixes.
    let tracked: Vec<rsleigh::Vn> = (0..8)
        .flat_map(|i| [reg(i * 16, 16), reg(i * 16, 8), reg(i * 16 + 8, 8)])
        .chain([uniq(0, 12), uniq(2, 18)])
        .collect();
    let survivors = dedup_overlapping_largest(&tracked);
    let queries: Vec<rsleigh::Vn> = (0..8)
        .flat_map(|i| [reg(i * 16, 4), reg(i * 16 + 8, 4), reg(i * 16, 16)])
        .chain([uniq(5, 4), uniq(0, 12), uniq(19, 1)])
        .collect();
    let cm = ContainerMap::build(&survivors, queries.iter().copied());
    for q in &queries {
        assert_eq!(
            cm.container_of(q),
            largest_container_in(&survivors, q),
            "{q:?}"
        );
    }
}

/// The subsumption test the sweep reduces to: an entry is dropped iff a
/// survivor already reaches past its end, or reaches exactly its end from
/// an EARLIER start (which makes that survivor strictly wider).
#[test]
fn dedup_drops_only_on_a_strictly_wider_reach() {
    // Same end, earlier start: the later entry is a suffix slice.
    assert_eq!(
        dedup_overlapping_largest(&[reg(0, 10), reg(2, 8)]),
        vec![reg(0, 10)]
    );
    // Same end, same start, same size: neither subsumes.
    assert_eq!(dedup_overlapping_largest(&[reg(2, 8), reg(2, 8)]).len(), 2);
    // A survivor reaching further than any later entry drops all of them.
    assert_eq!(
        dedup_overlapping_largest(&[reg(0, 16), reg(4, 4), reg(8, 8)]),
        vec![reg(0, 16)]
    );
    // Spaces are independent: a REGISTER container cannot drop a UNIQUE.
    assert_eq!(
        dedup_overlapping_largest(&[reg(0, 16), uniq(4, 4)]),
        vec![reg(0, 16), uniq(4, 4)]
    );
}

/// The map keeps its own copy of the tracked set, so a miss scans that,
/// whatever became of the caller's slice.
#[test]
fn container_map_falls_back_to_its_own_tracked_set() {
    let rax = reg(0, 8);
    let tracked = vec![rax];
    // No queries, so every lookup misses and falls through to the scan.
    let cm = ContainerMap::build(&tracked, std::iter::empty());
    drop(tracked);
    assert_eq!(cm.container_of(&reg(0, 4)), rax);
}

/// Exact past `u64::MAX`, half-open, and per space.
#[test]
fn vn_overlaps_is_exact_half_open_and_per_space() {
    assert_eq!(vn_end(&reg(u64::MAX, 4)), u128::from(u64::MAX) + 4);
    // A saturated end would put both ranges at `u64::MAX` and miss the
    // byte they share.
    assert!(vn_overlaps(&reg(u64::MAX, 4), &reg(u64::MAX - 1, 2)));
    assert!(vn_overlaps(&reg(0, 8), &reg(4, 2)));
    assert!(
        !vn_overlaps(&reg(0, 4), &reg(4, 4)),
        "adjacent ranges share no byte"
    );
    assert!(
        !vn_overlaps(&reg(4, 4), &reg(0, 4)),
        "adjacent ranges share no byte"
    );
    assert!(
        !vn_overlaps(&reg(0, 8), &uniq(0, 8)),
        "spaces never overlap"
    );
}
