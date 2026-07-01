//! Machine-register container resolution — the lifter's home for all
//! varnode-aliasing geometry.
//!
//! Overlapping registers (x86 `rax`/`eax`/`ax`/`al`, AArch64 `q0`/`d0`/`s0`, …)
//! are collapsed onto their largest tracked container at lift time. Everything
//! that reasons about that containment lives here:
//!
//! - [`seed_cc_regs`] / [`dedup_overlapping_largest`] build the canonical
//!   tracked universe handed to `strider_ir::FunctionBuilder::new`.
//! - [`build_container_map`] precomputes the O(1) `vn → container` lookup the
//!   register-aliasing hot path reads.
//! - [`largest_container_in`] is the linear-scan fallback for ad-hoc varnodes.
//!
//! This is deliberately NOT in the target-agnostic `strider-ir`: it is
//! machine-register knowledge owned by the lifter. `strider-ir` keeps a
//! `#[cfg(test/test-util)]` copy of the same geometry (see its
//! `canonicalize_tracked` / `largest_container_in`) only so fixtures can fake a
//! lifter; the two copies must stay in sync.

use rustc_hash::FxHashMap;

use strider_target::BuiltCallingConvention;

fn is_aliasable_space(space: rsleigh::VnSpace) -> bool {
    space == rsleigh::VnSpace::REGISTER || space == rsleigh::VnSpace::UNIQUE
}

/// Ensure every calling-convention register (int + float return regs, the
/// argument-passing regs, and the stack pointer) is present in `vns`, so a leaf
/// function that merely forwards a call still tracks each CC register the
/// aliasing-aware read path needs.  A wider view touched by the body is folded
/// away by [`dedup_overlapping_largest`] afterwards.
pub(crate) fn seed_cc_regs(vns: &mut Vec<rsleigh::Vn>, cc: &BuiltCallingConvention) {
    for v in cc
        .ret_val_regs
        .iter()
        .chain(cc.ret_val_regs_float.iter())
        .chain(cc.arg_passing_regs.iter())
        .chain(std::iter::once(&cc.stack_vn))
    {
        if !vns.contains(v) {
            vns.push(*v);
        }
    }
}

/// Filters `all_used_variables` down to the largest enclosing tracked variable
/// in each fixed-offset (REGISTER/UNIQUE) space (e.g. drop `edi` when `rdi` is
/// also touched).  CONST / code-space varnodes are kept verbatim —
/// containment-by-offset is meaningless there.  Returns survivors in INPUT
/// order; `strider_ir::Function::new` re-sorts before interning, so downstream
/// `InitialVnId` assignment is deterministic regardless of this order.
pub(crate) fn dedup_overlapping_largest(all_used_variables: &[rsleigh::Vn]) -> Vec<rsleigh::Vn> {
    fn end_of(v: &rsleigh::Vn) -> u64 {
        v.addr_off.saturating_add(u64::from(v.size))
    }

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

        let mut open: Vec<(u64, rsleigh::Vn)> = Vec::new();
        for (idx, v) in bucket {
            let v_end = end_of(&v);
            open.retain(|&(end, _)| end >= v.addr_off);
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

/// The linear-scan `vn → largest containing tracked vn` resolver (the fallback
/// behind the precomputed [`build_container_map`]).  A non-aliasable
/// (CONST / RAM / code) varnode, or one nothing tracked encloses, maps to
/// itself.
pub(crate) fn largest_container_in(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
    if !is_aliasable_space(vn.addr_space) {
        return *vn;
    }
    let start = vn.addr_off;
    let end = start.saturating_add(u64::from(vn.size));
    let mut best: Option<rsleigh::Vn> = None;
    for cand in vns {
        if cand.addr_space != vn.addr_space {
            continue;
        }
        let cs = cand.addr_off;
        let ce = cs.saturating_add(u64::from(cand.size));
        if cs > start || ce < end {
            continue;
        }
        if best.is_none_or(|b| b.size < cand.size) {
            best = Some(*cand);
        }
    }
    best.unwrap_or(*vn)
}

/// Build a `vn → largest containing tracked vn` map over `queries`, resolving
/// every REGISTER / UNIQUE query varnode against the `tracked` set with an
/// O(n log n) per-space stack sweep (never the O(n²) per-query rescan).
///
/// A query that is its own largest container maps to itself; a sub-register
/// slice maps to the largest tracked varnode that encloses it.  Non-aliasable
/// (CONST / RAM / code) query varnodes are omitted, so a lookup miss on them
/// falls through to the caller's fallback (self).  This is the O(1) fast path
/// the register-aliasing hot path reads on every register access.
pub(crate) fn build_container_map(
    tracked: &[rsleigh::Vn],
    queries: impl IntoIterator<Item = rsleigh::Vn>,
) -> FxHashMap<rsleigh::Vn, rsleigh::Vn> {
    let mut tracked_by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> = FxHashMap::default();
    for v in tracked {
        if is_aliasable_space(v.addr_space) {
            tracked_by_space.entry(v.addr_space).or_default().push(*v);
        }
    }

    let mut queries_by_space: FxHashMap<rsleigh::VnSpace, Vec<rsleigh::Vn>> = FxHashMap::default();
    let mut map: FxHashMap<rsleigh::Vn, rsleigh::Vn> = FxHashMap::default();
    for q in queries {
        if is_aliasable_space(q.addr_space) && !map.contains_key(&q) {
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

        let mut active: Vec<rsleigh::Vn> = Vec::new();
        let mut ti = 0usize;
        for q in qs {
            let q_start = q.addr_off;
            let q_end = q_start.saturating_add(u64::from(q.size));
            while ti < opens.len() && opens[ti].addr_off <= q_start {
                active.push(opens[ti]);
                ti += 1;
            }
            active.retain(|c| c.addr_off.saturating_add(u64::from(c.size)) >= q_start);
            let container = active
                .iter()
                .filter(|c| c.addr_off.saturating_add(u64::from(c.size)) >= q_end)
                .max_by_key(|c| c.size)
                .copied()
                .unwrap_or(q);
            map.insert(q, container);
        }
    }
    map
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

    #[test]
    fn dedup_drops_enclosed_and_keeps_wider() {
        let rdi = reg(0, 8);
        let edi = reg(0, 4);
        assert_eq!(dedup_overlapping_largest(&[rdi, edi]), vec![rdi]);
        assert_eq!(dedup_overlapping_largest(&[edi, rdi]), vec![rdi]);
    }

    #[test]
    fn dedup_keeps_partial_overlap() {
        let a = reg(0, 8);
        let b = reg(4, 8);
        assert_eq!(dedup_overlapping_largest(&[a, b]), vec![a, b]);
    }

    #[test]
    fn largest_container_resolves_subregister() {
        let rax = reg(0, 8);
        let eax = reg(0, 4);
        assert_eq!(largest_container_in(&[rax], &eax), rax);
        // Nothing tracked encloses a disjoint reg → self.
        let other = reg(16, 4);
        assert_eq!(largest_container_in(&[rax], &other), other);
    }

    #[test]
    fn build_container_map_picks_widest_crossing_encloser() {
        // Crossing partial-overlap enclosers: two varnodes that each enclose a
        // third but neither encloses the other.  The dropped inner view must map
        // to the WIDER encloser, not merely the first-seen one — the case a
        // naive first-open stack sweep returned too small.
        fn uniq(off: u64, size: u32) -> rsleigh::Vn {
            rsleigh::Vn {
                size,
                addr_off: off,
                addr_space: rsleigh::VnSpace::UNIQUE,
            }
        }
        let a = uniq(0, 12); // [0,12): encloses [5,9); crosses b; survives.
        let b = uniq(2, 18); // [2,20): encloses [5,9) and is wider; survives.
        let inner = uniq(5, 4); // [5,9): enclosed by BOTH a and b -> dropped.

        let survivors = dedup_overlapping_largest(&[a, b, inner]);
        assert_eq!(
            survivors,
            vec![a, b],
            "crossing enclosers both survive (neither encloses the other); inner dropped"
        );

        let map = build_container_map(&survivors, [a, b, inner]);
        assert_eq!(
            map[&inner], b,
            "inner maps to the WIDER (size-18) encloser b, not the size-12 a"
        );
        assert_eq!(map[&a], a, "a is its own container");
        assert_eq!(map[&b], b, "b is its own container");
    }

    #[test]
    fn seed_adds_missing_cc_regs() {
        let cc = BuiltCallingConvention {
            arg_passing_regs: vec![reg(56, 8)],
            ret_val_regs: vec![reg(0, 8)],
            ..BuiltCallingConvention::default()
        };
        let mut vns = vec![reg(0, 8)];
        seed_cc_regs(&mut vns, &cc);
        assert!(vns.contains(&reg(56, 8)));
        assert!(vns.contains(&cc.stack_vn));
    }
}
