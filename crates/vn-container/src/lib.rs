//! Varnode container geometry — the single home for register-aliasing
//! containment reasoning.
//!
//! Overlapping machine registers (x86 `rax`/`eax`/`ax`/`al`, AArch64
//! `q0`/`d0`/`s0`, x87 `ST*`, …) are collapsed onto their largest tracked
//! *container*. Everything that reasons about that containment lives here, so
//! neither the target-agnostic IR nor the lifter owns (or duplicates) it:
//!
//! - [`dedup_overlapping_largest`] builds a canonical tracked set (drop every
//!   varnode strictly enclosed by a wider same-space one) — used when
//!   constructing a function's tracked-variable universe.
//! - [`ContainerMap`] precomputes the O(1) `vn → container` lookup the
//!   register-aliasing hot path reads, with [`largest_container_in`] as the
//!   linear-scan fallback for ad-hoc varnodes.
//! - [`vn_contains`] is the pairwise "does A enclose B" test pattern matching
//!   uses to relate a pinned sub-register to the container the IR stores.
//!
//! The crate is a pure-geometry leaf: it depends only on `rsleigh` for the
//! [`rsleigh::Vn`] varnode type and reasons entirely about `(space, offset,
//! size)` ranges. It knows nothing about calling conventions or the IR graph.

use rustc_hash::FxHashMap;

fn is_aliasable_space(space: rsleigh::VnSpace) -> bool {
    space == rsleigh::VnSpace::REGISTER || space == rsleigh::VnSpace::UNIQUE
}

fn end_of(v: &rsleigh::Vn) -> u64 {
    // Saturating: high-offset CR slices on ppc64 / aarch64be can push
    // `addr_off + size` past `u64::MAX`.
    v.addr_off.saturating_add(u64::from(v.size))
}

/// True when `outer` fully encloses `inner` within the same aliasable space.
///
/// Pure pairwise geometry — no tracked-set / universe needed. Used by pattern
/// matching to match a pinned sub-register (`eax`) against the largest
/// container the IR stores (`rax`).
pub fn vn_contains(outer: &rsleigh::Vn, inner: &rsleigh::Vn) -> bool {
    outer.addr_space == inner.addr_space
        && outer.addr_off <= inner.addr_off
        && end_of(outer) >= end_of(inner)
}

/// Filters `all_used_variables` down to the largest enclosing tracked variable
/// in each fixed-offset (REGISTER/UNIQUE) space (e.g. drop `edi` when `rdi` is
/// also touched). CONST / code-space varnodes are kept verbatim —
/// containment-by-offset is meaningless there.
///
/// Returns survivors in INPUT order; callers that need deterministic id
/// assignment sort afterwards. A varnode is dropped iff some STRICTLY larger
/// same-space varnode encloses its byte range.
///
/// MIPS-style example: Sleigh's MIPS lifter writes a 64-bit IntMul result to a
/// unique varnode then Copies a 4-byte slice to a register; without this filter
/// the 4-byte and 8-byte unique varnodes look like independent SSA variables.
/// Keeping the wider varnode preserves the data dependency.
pub fn dedup_overlapping_largest(all_used_variables: &[rsleigh::Vn]) -> Vec<rsleigh::Vn> {
    // Bucket aliasable inputs by space, carrying each entry's original index.
    let mut by_space: FxHashMap<rsleigh::VnSpace, Vec<(usize, rsleigh::Vn)>> = FxHashMap::default();
    for (i, v) in all_used_variables.iter().enumerate() {
        if is_aliasable_space(v.addr_space) {
            by_space.entry(v.addr_space).or_default().push((i, *v));
        }
    }

    let mut dropped = vec![false; all_used_variables.len()];
    for (_space, mut bucket) in by_space {
        // addr_off ascending, then size descending so a wider enclosure is seen
        // before the narrower slices it contains.
        bucket.sort_by_key(|(_, v)| (v.addr_off, std::cmp::Reverse(v.size)));

        // Open enclosures whose range still extends past the current start,
        // kept as `(end, vn)` and holding only SURVIVORS.
        let mut open: Vec<(u64, rsleigh::Vn)> = Vec::new();
        for (idx, v) in bucket {
            let v_end = end_of(&v);
            // Drop opens whose range ends before this entry STARTS: by the
            // addr_off-ascending sort every remaining entry starts at or after
            // `v.addr_off`, so such an open can enclose neither `v` nor any
            // later entry.
            open.retain(|&(end, _)| end >= v.addr_off);
            // Strictly-larger enclosing open (`off ≤ v.off` by sort,
            // `end ≥ v_end`, `size > v.size`): `v` is a subsumed sub-register
            // view and is dropped; else it is the largest in its chain.
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
        .filter(|(i, _)| !dropped[*i])
        .map(|(_, v)| *v)
        .collect()
}

/// Largest varnode in `vns` (same REGISTER/UNIQUE space, offset-range
/// inclusion) that fully contains `vn`, or `vn` itself when none does.
///
/// The linear-scan fallback behind [`ContainerMap`]. A non-aliasable
/// (CONST / RAM / code) varnode maps to itself — containment-by-offset is
/// meaningless there.
pub fn largest_container_in(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
    if !is_aliasable_space(vn.addr_space) {
        return *vn;
    }
    let end = end_of(vn);
    let mut best: Option<rsleigh::Vn> = None;
    for cand in vns {
        if cand.addr_space != vn.addr_space {
            continue;
        }
        if cand.addr_off > vn.addr_off || end_of(cand) < end {
            continue;
        }
        if best.is_none_or(|b| b.size < cand.size) {
            best = Some(*cand);
        }
    }
    best.unwrap_or(*vn)
}

/// A precomputed `vn → largest containing tracked vn` map: the O(1) fast path
/// the register-aliasing hot path reads on every register access.
///
/// Built once per function from the tracked set plus every queried varnode.
/// A lookup miss (an ad-hoc varnode absent from the map, or a non-aliasable
/// one) falls through to the [`largest_container_in`] linear scan via
/// [`ContainerMap::container_of`].
#[derive(Debug, Clone, Default)]
pub struct ContainerMap {
    map: FxHashMap<rsleigh::Vn, rsleigh::Vn>,
}

impl ContainerMap {
    /// Resolve every REGISTER / UNIQUE `queries` varnode against the `tracked`
    /// set with an O(n log n) per-space stack sweep (never an O(n²) per-query
    /// rescan). A query that is its own largest container maps to itself; a
    /// sub-register slice maps to the largest tracked varnode that encloses it.
    /// Non-aliasable (CONST / RAM / code) queries are omitted.
    pub fn build(tracked: &[rsleigh::Vn], queries: impl IntoIterator<Item = rsleigh::Vn>) -> Self {
        // Bucket the tracked set by space, `(off ascending, size descending)`.
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
                // Mark seen (self placeholder); the sweep fills the real value.
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

            // Two-pointer sweep keeping the active enclosure window; opens are
            // small (a register file), so this is O((t + q) log(t + q)).
            let mut active: Vec<rsleigh::Vn> = Vec::new();
            let mut ti = 0usize;
            for q in qs {
                let q_start = q.addr_off;
                let q_end = end_of(&q);
                while ti < opens.len() && opens[ti].addr_off <= q_start {
                    active.push(opens[ti]);
                    ti += 1;
                }
                active.retain(|c| end_of(c) >= q_start);
                let container = active
                    .iter()
                    .filter(|c| end_of(c) >= q_end)
                    .max_by_key(|c| c.size)
                    .copied()
                    .unwrap_or(q);
                map.insert(q, container);
            }
        }
        Self { map }
    }

    /// Resolve `vn` to its largest tracked container: the precomputed map hit,
    /// else an on-the-fly [`largest_container_in`] scan of `tracked` for an
    /// ad-hoc varnode not in the map. Returns `vn` when nothing tracked
    /// contains it (or it is non-aliasable).
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
        assert!(vn_contains(&rax, &reg(0, 4))); // eax ⊆ rax
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
        // Partial overlap (neither encloses the other): both survive.
        assert_eq!(dedup_overlapping_largest(&[reg(0, 8), reg(4, 8)]), vec![
            reg(0, 8),
            reg(4, 8)
        ]);
        // Equal-size aliases and exact duplicates both survive.
        assert_eq!(dedup_overlapping_largest(&[reg(0, 4), reg(0, 4)]).len(), 2);
        assert!(dedup_overlapping_largest(&[]).is_empty());
    }

    #[test]
    fn dedup_overflow_safe_on_high_offset() {
        let wide = reg(u64::MAX - 4, 8);
        let narrow = reg(u64::MAX - 4, 2);
        assert_eq!(dedup_overlapping_largest(&[wide, narrow]), vec![wide]);
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
        // Crossing partial-overlap enclosers: two varnodes that each enclose a
        // third but neither encloses the other. The dropped inner view must map
        // to the WIDER encloser, not merely the first-seen one.
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
        // Map built with no queries: an ad-hoc lookup misses and falls back.
        let cm = ContainerMap::build(&[rax], std::iter::empty());
        assert_eq!(cm.container_of(&[rax], &reg(0, 4)), rax);
    }
}
