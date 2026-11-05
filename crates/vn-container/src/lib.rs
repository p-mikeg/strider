use rustc_hash::FxHashMap;

#[cfg(test)]
mod tests;

/// The spaces whose varnodes nest: REGISTER (`eax` inside `rax`) and UNIQUE
/// (a Sleigh temporary sliced by a later op). Every other space is flat.
#[must_use]
pub fn is_aliasable_space(space: rsleigh::VnSpace) -> bool {
    space == rsleigh::VnSpace::REGISTER || space == rsleigh::VnSpace::UNIQUE
}

/// One past `v`'s last byte, exact in `u128`.
pub fn vn_end(v: &rsleigh::Vn) -> u128 {
    u128::from(v.addr_off) + u128::from(v.size)
}

/// True when `outer` encloses `inner` in the same space.
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

/// Drops each REGISTER / UNIQUE varnode a strictly larger same-space varnode
/// encloses (`edi` when `rdi` is present); other spaces pass through.
/// Survivors keep input order. Partially overlapping varnodes both survive;
/// none arise, as register files nest and UNIQUE temps sit in 256-byte-aligned
/// blocks.
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

        // Every earlier entry starts at or before `v`, so one reaching `v_end`
        // encloses `v`, strictly unless it has `v`'s exact range.
        let mut max_end: u128 = 0;
        let mut earliest_at_max_end: u64 = u64::MAX;
        for (idx, v) in bucket {
            let v_end = vn_end(&v);
            let enclosed =
                max_end > v_end || (max_end == v_end && earliest_at_max_end < v.addr_off);
            // Advances on strict growth only, or byte-identical varnodes
            // drop each other.
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

/// Largest same-space varnode in `vns` containing `vn`, else `vn` itself;
/// always `vn` in a flat space.
pub fn largest_container_in(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> rsleigh::Vn {
    if !is_aliasable_space(vn.addr_space) {
        return *vn;
    }
    vns.iter()
        .filter(|c| vn_contains(c, vn))
        // `addr_off` breaks a size tie so `ContainerMap`, which scans another
        // order, agrees.
        .max_by_key(|c| (c.size, c.addr_off))
        .copied()
        .unwrap_or(*vn)
}

/// Smallest same-space varnode in `vns` enclosing `vn`; `None` in a flat space
/// or on a miss.
pub fn smallest_enclosing(vns: &[rsleigh::Vn], vn: &rsleigh::Vn) -> Option<rsleigh::Vn> {
    if !is_aliasable_space(vn.addr_space) {
        return None;
    }
    vns.iter()
        .filter(|c| vn_contains(c, vn))
        .min_by_key(|c| (c.size, c.addr_off))
        .copied()
}

/// `vn -> container` lookup over a tracked set; a miss scans the set.
#[derive(Debug, Clone)]
pub struct ContainerMap {
    tracked: Box<[rsleigh::Vn]>,
    map: FxHashMap<rsleigh::Vn, rsleigh::Vn>,
}

impl ContainerMap {
    /// Resolves each REGISTER / UNIQUE query against `tracked` in one
    /// O(n log n) sweep per space.
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

            // Two-pointer sweep; `active` holds the containers still open at
            // `q`'s start.
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

    /// The largest tracked container of `vn`, or `vn` itself.
    pub fn container_of(&self, vn: &rsleigh::Vn) -> rsleigh::Vn {
        if let Some(c) = self.map.get(vn) {
            return *c;
        }
        largest_container_in(&self.tracked, vn)
    }
}
