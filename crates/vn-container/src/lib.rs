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
/// the other. No register file checked has one: they all nest exactly.
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

        // Enclosures still extending past the current start, SURVIVORS only.
        let mut open: Vec<(u128, rsleigh::Vn)> = Vec::new();
        for (idx, v) in bucket {
            let v_end = end_of(&v);
            // Every remaining entry starts at or after `v.addr_off`, so an open
            // ending before it can enclose neither `v` nor anything later.
            open.retain(|&(end, _)| end >= u128::from(v.addr_off));
            // `off <= v.off` already holds by the sort, so a strictly wider
            // open reaching `v_end` makes `v` a subsumed sub-register view.
            let enclosed = open.iter().any(|&(end, c)| end >= v_end && c.size > v.size);
            if enclosed {
                dropped[idx] = true;
            } else {
                open.push((v_end, v));
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
    let end = end_of(vn);
    vns.iter()
        .filter(|c| c.addr_space == vn.addr_space && c.addr_off <= vn.addr_off && end_of(c) >= end)
        // `addr_off` breaks an equal-size tie so this agrees with
        // `ContainerMap`, which scans a differently ordered list. Two resolvers
        // disagreeing would put a read and a write of one varnode under
        // different SSA variables.
        .max_by_key(|c| (c.size, c.addr_off))
        .copied()
        .unwrap_or(*vn)
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

            // Two-pointer sweep over the active enclosure window. Each query
            // scans `active`, which holds only the containers open at that
            // address and is register-file sized, so the sweep is O(q) after
            // the two sorts.
            let mut active: Vec<rsleigh::Vn> = Vec::new();
            let mut ti = 0usize;
            for q in qs {
                let q_start = q.addr_off;
                let q_end = end_of(&q);
                while ti < opens.len() && opens[ti].addr_off <= q_start {
                    active.push(opens[ti]);
                    ti += 1;
                }
                active.retain(|c| end_of(c) >= u128::from(q_start));
                let container = active
                    .iter()
                    .filter(|c| end_of(c) >= q_end)
                    .max_by_key(|c| (c.size, c.addr_off))
                    .copied()
                    .unwrap_or(q);
                map.insert(q, container);
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

    #[test]
    fn container_map_falls_back_to_linear_scan_for_adhoc() {
        let rax = reg(0, 8);
        // No queries, so every lookup misses and falls through to the scan.
        let cm = ContainerMap::build(&[rax], std::iter::empty());
        assert_eq!(cm.container_of(&[rax], &reg(0, 4)), rax);
    }
}
