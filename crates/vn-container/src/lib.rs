use rustc_hash::FxHashMap;

#[cfg(test)]
mod tests;

/// The two spaces whose varnodes overlap, so a read or write of one may name
/// part of another: REGISTER (x86 `eax` inside `rax`) and UNIQUE (a Sleigh
/// temporary sliced by a later op). Every other space is flat, and the
/// container machinery in this crate answers `None` there.
///
/// The one place the list lives: a tracked universe that disagreed with it
/// would drop the varnodes the lift then fails to read.
#[must_use]
pub fn is_aliasable_space(space: rsleigh::VnSpace) -> bool {
    space == rsleigh::VnSpace::REGISTER || space == rsleigh::VnSpace::UNIQUE
}

/// One past `v`'s last byte. `u128`, so the sum is exact: saturating at
/// `u64::MAX` would report a non-containing pair as contained.
pub fn vn_end(v: &rsleigh::Vn) -> u128 {
    u128::from(v.addr_off) + u128::from(v.size)
}

/// True when `outer` fully encloses `inner` in the same aliasable space.
/// Pairwise only, so it needs no tracked set.
pub fn vn_contains(outer: &rsleigh::Vn, inner: &rsleigh::Vn) -> bool {
    outer.addr_space == inner.addr_space
        && outer.addr_off <= inner.addr_off
        && vn_end(outer) >= vn_end(inner)
}

/// True when `a` and `b` share a byte in the same address space.
pub fn vn_overlaps(a: &rsleigh::Vn, b: &rsleigh::Vn) -> bool {
    a.addr_space == b.addr_space
        && u128::from(a.addr_off) < vn_end(b)
        && u128::from(b.addr_off) < vn_end(a)
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
/// [`smallest_enclosing`] cannot seat in a declared register. UNIQUE nests
/// too, by a different construction: `SleighCompile` hands out temporaries on
/// a 256-byte stride and rejects a constructor temp wider than that, and the
/// per-instruction uniquifier only ORs bits at or above 8, so every UNIQUE
/// varnode is one 256-aligned block and a crossing pair cannot arise. The
/// residual caveat is that the size check reads the DECLARED size, leaving an
/// operand-dependent temp unchecked, bounded in practice by the widest
/// selectable register.
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
            let v_end = vn_end(&v);
            let enclosed =
                max_end > v_end || (max_end == v_end && earliest_at_max_end < v.addr_off);
            // `earliest_at_max_end` advances on STRICT growth only: it has to
            // stay the earliest start reaching `max_end`, or byte-identical
            // varnodes drop each other.
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
        // An equal-size tie needs two PARTIALLY overlapping enclosers, which no
        // declared register file has; `addr_off` only makes the pick total.
        .min_by_key(|c| (c.size, c.addr_off))
        .copied()
}

/// O(1) `vn -> container` lookup for the register-aliasing reads, built once
/// per function over the tracked set, which it keeps: a miss falls back to a
/// linear scan of it.
#[derive(Debug, Clone)]
pub struct ContainerMap {
    tracked: Box<[rsleigh::Vn]>,
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
                active.retain(|c| vn_end(c) >= u128::from(q_start));
                map.insert(q, largest_container_in(&active, &q));
            }
        }
        Self {
            tracked: tracked.into(),
            map,
        }
    }

    /// Map hit, else an on-the-fly [`largest_container_in`] scan for an ad-hoc
    /// varnode. Returns `vn` when nothing tracked contains it.
    pub fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        if let Some(c) = self.map.get(vn) {
            return *c;
        }
        largest_container_in(&self.tracked, vn)
    }
}
