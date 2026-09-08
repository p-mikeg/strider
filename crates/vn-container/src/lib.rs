use rustc_hash::FxHashMap;

fn is_aliasable_space(space: rsleigh::VnSpace) -> bool {
    space == rsleigh::VnSpace::REGISTER || space == rsleigh::VnSpace::UNIQUE
}

// `u128` so the sum is exact: saturating at `u64::MAX` would report a
// non-containing pair as contained.
fn end_of(v: &rsleigh::Vn) -> u128 {
    u128::from(v.addr_off) + u128::from(v.size)
}

/// True when `outer` fully encloses `inner` in the same aliasable space.
/// Pairwise only, so it needs no tracked set.
pub fn vn_contains(outer: &rsleigh::Vn, inner: &rsleigh::Vn) -> bool {
    outer.addr_space == inner.addr_space
        && outer.addr_off <= inner.addr_off
        && end_of(outer) >= end_of(inner)
}

/// Keeps only the largest enclosing varnode per REGISTER/UNIQUE range (drop
/// `edi` when `rdi` is also touched); CONST / code-space varnodes pass through
/// since containment-by-offset is meaningless there.
///
/// A varnode is dropped iff some STRICTLY larger same-space varnode encloses
/// its byte range. Survivors come back in INPUT order, so callers wanting
/// deterministic id assignment must sort afterwards.
///
/// Collapsing to the widest varnode is what preserves the data dependency when
/// a lifter writes a wide unique then copies a narrow slice out of it;
/// otherwise the two views look like independent SSA variables. Only
/// CONTAINMENT collapses: two PARTIALLY overlapping varnodes both survive and
/// are modelled as non-aliasing, so a write to one is invisible to a read of
/// the other. No register file checked declares one: they all nest exactly.
/// That discharges the REGISTER half; a computed register slot is held to the
/// same shape upstream by `strider-lift`'s `register_slot`, which drops a slot
/// [`smallest_enclosing`] cannot seat in a declared register. UNIQUE, a
/// temporary arena, carries no such guarantee: a crossing pair of temporaries
/// survives as two independent SSA variables.
pub fn dedup_overlapping_largest(all_used_variables: &[rsleigh::Vn]) -> Vec<rsleigh::Vn> {
    let mut by_space: FxHashMap<rsleigh::VnSpace, Vec<(usize, rsleigh::Vn)>> = FxHashMap::default();
    for (i, v) in all_used_variables.iter().enumerate() {
        if is_aliasable_space(v.addr_space) {
            by_space.entry(v.addr_space).or_default().push((i, *v));
        }
    }

    let mut dropped = vec![false; all_used_variables.len()];
    for (_space, mut bucket) in by_space {
        // Size descending within an offset so a wider enclosure is seen before
        // the narrower slices it contains.
        bucket.sort_by_key(|(_, v)| (v.addr_off, std::cmp::Reverse(v.size)));

        // Widest reach among the survivors seen so far, and the earliest start
        // achieving it. Every survivor starts at or before the current entry
        // by the sort, so one reaching `v_end` encloses `v`; it is STRICTLY
        // wider unless it spans `v`'s exact range, which the start
        // distinguishes. No open LIST: a shorter reach can never subsume where
        // the widest does not, so an input of byte-identical varnodes cannot
        // grow the state.
        let mut max_end: u128 = 0;
        let mut earliest_at_max_end: u64 = u64::MAX;
        for (idx, v) in bucket {
            let v_end = end_of(&v);
            let enclosed =
                max_end > v_end || (max_end == v_end && earliest_at_max_end < v.addr_off);
            if enclosed {
                dropped[idx] = true;
            } else if v_end > max_end {
                max_end = v_end;
                earliest_at_max_end = v.addr_off;
            }
        }
    }

    all_used_variables
        .iter()
        .enumerate()
        .filter_map(|(i, v)| (!dropped[i]).then_some(*v))
        .collect()
}

/// Largest same-space varnode in `vns` containing `vn`, else `vn` itself.
/// A non-aliasable (CONST / RAM / code) varnode always maps to itself.
pub fn largest_container_in(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
    if !is_aliasable_space(vn.addr_space) {
        return *vn;
    }
    vns.iter()
        .filter(|c| vn_contains(c, vn))
        // `addr_off` breaks an equal-size tie so this agrees with
        // `ContainerMap`, which scans a differently ordered list. Two resolvers
        // disagreeing would put a read and a write of one varnode under
        // different SSA variables.
        .max_by_key(|c| (c.size, c.addr_off))
        .copied()
        .unwrap_or(*vn)
}

/// Smallest same-space varnode in `vns` enclosing `vn`, or `None` when none
/// does. Unlike [`largest_container_in`] a miss is reported rather than
/// answered with `vn` itself: the caller is asking whether `vn` is a slice of
/// something `vns` declares, and a computed offset need not be.
///
/// Smallest, because the answer names the register a computed address reaches:
/// the widest enclosing view would over-state which bytes the access covers.
pub fn smallest_enclosing(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> Option<rsleigh::Vn> {
    if !is_aliasable_space(vn.addr_space) {
        return None;
    }
    vns.iter()
        .filter(|c| vn_contains(c, vn))
        // `addr_off` breaks an equal-size tie for the same reason
        // `largest_container_in` does: two resolvers disagreeing would put a
        // read and a write of one varnode under different SSA variables.
        .min_by_key(|c| (c.size, c.addr_off))
        .copied()
}

/// O(1) `vn -> container` lookup for the register-aliasing reads, built once
/// per function. A miss falls back to a linear scan.
#[derive(Debug, Clone, Default)]
pub struct ContainerMap {
    map: FxHashMap<rsleigh::Vn, rsleigh::Vn>,
}

impl ContainerMap {
    /// Resolves every REGISTER / UNIQUE query against `tracked` with a
    /// per-space sweep: O(n log n), never an O(n²) per-query rescan.
    /// Non-aliasable (CONST / RAM / code) queries are omitted entirely.
    pub fn build(tracked: &[rsleigh::Vn], queries: impl IntoIterator<Item = rsleigh::Vn>) -> Self {
        let mut tracked_by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> =
            FxHashMap::default();
        for v in tracked {
            if is_aliasable_space(v.addr_space) {
                tracked_by_space.entry(v.addr_space).or_default().push(*v);
            }
        }

        let mut queries_by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> =
            FxHashMap::default();
        let mut map: FxHashMap<rsleigh::Vn, rsleigh::Vn> = FxHashMap::default();
        for q in queries {
            if is_aliasable_space(q.addr_space) && !map.contains_key(&q) {
                // Self is a placeholder marking `q` seen; the sweep overwrites
                // it with the real container.
                map.insert(q, q);
                queries_by_space.entry(q.addr_space).or_default().push(q);
            }
        }

        for (space, mut qs) in queries_by_space {
            let Some(tracked_here) = tracked_by_space.get(&space) else {
                continue;
            };
            let mut opens: Vec<rsleigh::Vn> = tracked_here.clone();
            opens.sort_by_key(|v| (v.addr_off, std::cmp::Reverse(v.size)));
            qs.sort_by_key(|q| (q.addr_off, std::cmp::Reverse(q.size)));

            // Two-pointer sweep over the active enclosure window. `retain`
            // prunes it to the containers still open at `q`'s start, keeping
            // it register-file sized, so the sweep is O(q) after the two
            // sorts. `largest_container_in` does the selection, which is what
            // keeps the two resolvers from disagreeing.
            let mut active: Vec<rsleigh::Vn> = Vec::new();
            let mut ti = 0usize;
            for q in qs {
                let q_start = q.addr_off;
                while ti < opens.len() && opens[ti].addr_off <= q_start {
                    active.push(opens[ti]);
                    ti += 1;
                }
                active.retain(|c| end_of(c) >= u128::from(q_start));
                map.insert(q, largest_container_in(&active, &q));
            }
        }
        Self { map }
    }

    /// Map hit, else an on-the-fly [`largest_container_in`] scan for an ad-hoc
    /// varnode. Returns `vn` when nothing tracked contains it.
    pub fn container_of(&self, tracked: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
        if let Some(c) = self.map.get(vn) {
            return *c;
        }
        largest_container_in(tracked, vn)
    }
}

#[cfg(test)]
mod tests {
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

    /// Both ends run past `u64::MAX`, so a saturating `end_of` reports them
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
        assert_eq!(cm.container_of(&survivors, &inner), b, "widest encloser");
        assert_eq!(cm.container_of(&survivors, &a), a);
        assert_eq!(cm.container_of(&survivors, &b), b);
    }

    /// Byte-identical varnodes never subsume one another (enclosure is
    /// STRICT), so an open-list sweep keeps every one of them live and the
    /// scan turns quadratic.
    #[test]
    fn dedup_stays_linear_on_byte_identical_varnodes() {
        fn run(n: usize) -> std::time::Duration {
            let input = vec![reg(0, 4); n];
            let start = std::time::Instant::now();
            let out = dedup_overlapping_largest(&input);
            assert_eq!(out.len(), n, "equal-size duplicates all survive");
            start.elapsed()
        }
        run(2_000);
        let small = run(8_000);
        let large = run(64_000);
        // Linear would be 8x; quadratic 64x. Loose enough to survive a loaded
        // machine, tight enough to fail the open-list shape.
        assert!(
            large.as_secs_f64() < small.as_secs_f64() * 24.0,
            "8x the input cost {:.1}x ({small:?} -> {large:?})",
            large.as_secs_f64() / small.as_secs_f64(),
        );
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
                cm.container_of(&survivors, q),
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

    #[test]
    fn container_map_falls_back_to_linear_scan_for_adhoc() {
        let rax = reg(0, 8);
        // No queries, so every lookup misses and falls through to the scan.
        let cm = ContainerMap::build(&[rax], std::iter::empty());
        assert_eq!(cm.container_of(&[rax], &reg(0, 4)), rax);
    }
}
