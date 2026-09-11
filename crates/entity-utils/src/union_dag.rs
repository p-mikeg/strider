use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{EntityList, EntityRef, ListPool, PrimaryMap, SecondaryMap, entity_impl};
use rustc_hash::FxHashSet;
use std::hash::Hash;

/// Never exposed; callers address the DAG by their own external key `N`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct UnionId(u32);
entity_impl!(UnionId);

#[derive(Clone, Debug)]
struct Node<V> {
    /// `None` for a pure join node, one [`UnionDag::union`] made for a key
    /// that had no value of its own yet.
    own: Option<V>,
    parents: EntityList<UnionId>,
}

#[derive(Clone, Debug)]
pub struct UnionDag<N: EntityRef, V: Copy + Eq + Hash> {
    /// `NONE` means the key has no set yet.
    roots: SecondaryMap<N, PackedOption<UnionId>>,
    nodes: PrimaryMap<UnionId, Node<V>>,
    links: ListPool<UnionId>,
    /// The `(dst, src)` pairs already linked, one entry per DISTINCT link.
    linked: FxHashSet<(UnionId, UnionId)>,
    /// The `(root, value)` pairs already extended, one entry per DISTINCT value.
    held: FxHashSet<(UnionId, V)>,
}

impl<N: EntityRef, V: Copy + Eq + Hash> Default for UnionDag<N, V> {
    fn default() -> Self {
        Self {
            roots: SecondaryMap::new(),
            nodes: PrimaryMap::new(),
            links: ListPool::new(),
            linked: FxHashSet::default(),
            held: FxHashSet::default(),
        }
    }
}

impl<N: EntityRef, V: Copy + Eq + Hash> UnionDag<N, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// O(1) amortised: the first value fills `n`'s own node, later ones become
    /// absorbed leaves. Re-adding a value already held on `n`'s OWN root is a
    /// no-op; one `n` reaches only through a union is allocated again, and
    /// [`Self::for_each`] then yields it twice.
    pub fn extend(&mut self, n: N, v: V) {
        let root = self.ensure(n);
        // Same reason `union` keeps `linked`: re-adding one value would grow
        // `root`'s parents without bound and turn `for_each` linear in the
        // number of `extend` calls, its `seen` set hiding the repetition in the
        // ANSWER, not in the COST. Keyed by the pair rather than by `own`
        // alone, so alternating values do not each defeat the guard.
        if !self.held.insert((root, v)) {
            return;
        }
        if self.nodes[root].own.is_none() {
            self.nodes[root].own = Some(v);
        } else {
            let leaf = self.alloc(Some(v));
            self.nodes[root].parents.push(leaf, &mut self.links);
        }
    }

    /// O(1) amortised: links `src`'s root under `dst` rather than copying. A no-op when
    /// `src` is empty.
    ///
    /// The link is a LIVE ALIAS, not a snapshot: a value added to `src` AFTER
    /// the union is visible from `dst` too. Not the other way round: `dst`'s
    /// own later values stay out of `src`.
    pub fn union(&mut self, dst: N, src: N) {
        let Some(src_root) = self.roots[src].expand() else {
            return;
        };
        let dst_root = self.ensure(dst);
        // Re-linking one `(dst, src)` pair would otherwise grow `dst`'s
        // parents without bound and turn `for_each` linear in the number of
        // `union` calls: its `seen` set hides the repetition in the ANSWER,
        // not in the COST. `linked` catches the pair whatever else was unioned
        // in between, so `parents` holds one entry per distinct link.
        if !self.linked.insert((dst_root, src_root)) {
            return;
        }
        self.nodes[dst_root].parents.push(src_root, &mut self.links);
    }

    pub fn is_empty(&self, n: N) -> bool {
        self.roots[n].is_none()
    }

    /// Visits every value reachable from `n`'s set. Dedup is per NODE, not per
    /// value: a shared sub-DAG is walked once, but the same value held by two
    /// nodes is yielded twice and the caller must collect it. Cycle-safe:
    /// mutual absorption still terminates.
    ///
    /// Costs the transitive closure reached from `n`, not the count of values
    /// yielded. On a chain of unions each key reaches the whole chain below it,
    /// so sweeping every key is quadratic in the chain length.
    pub fn for_each(&self, n: N, mut f: impl FnMut(V)) {
        let Some(root) = self.roots[n].expand() else {
            return;
        };
        // Sized by what the walk VISITS. A dense set would zero a backing
        // vector reaching the largest `UnionId` in the arena, making a call
        // that yields one value cost in proportion to the whole arena.
        let mut seen: FxHashSet<UnionId> = FxHashSet::default();
        let mut stack = vec![root];
        seen.insert(root);
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if let Some(v) = node.own {
                f(v);
            }
            for &parent in node.parents.as_slice(&self.links) {
                if seen.insert(parent) {
                    stack.push(parent);
                }
            }
        }
    }

    /// Relabels keys after the `N` space is compacted. `f` must be INJECTIVE:
    /// two keys mapping to one keep only whichever the iteration order writes
    /// last.
    ///
    /// `f` returning `None` culls a key's entry point, so a direct lookup of
    /// it is empty. Its DAG node survives: a surviving key that unioned from
    /// it still reaches those values. Only the key->root map is rebuilt; the
    /// DAG arena is untouched, which is why `held` / `linked` need no pruning:
    /// a `UnionId` is never reused, so a stale pair can never match a new one.
    pub fn remap(&mut self, f: impl Fn(N) -> Option<N>) {
        let mut roots: SecondaryMap<N, PackedOption<UnionId>> = SecondaryMap::new();
        for (key, root) in self.roots.iter() {
            if let Some(root) = root.expand()
                && let Some(new_key) = f(key)
            {
                roots[new_key] = root.into();
            }
        }
        self.roots = roots;
    }

    /// Creates a valueless join node when `n` has no root yet.
    fn ensure(&mut self, n: N) -> UnionId {
        match self.roots[n].expand() {
            Some(root) => root,
            None => {
                let id = self.alloc(None);
                self.roots[n] = id.into();
                id
            }
        }
    }

    fn alloc(&mut self, own: Option<V>) -> UnionId {
        self.nodes.push(Node {
            own,
            parents: EntityList::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashSet;

    #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
    struct Key(u32);
    entity_impl!(Key);

    fn set_of(dag: &UnionDag<Key, u64>, k: Key) -> FxHashSet<u64> {
        let mut s = FxHashSet::default();
        dag.for_each(k, |v| {
            s.insert(v);
        });
        s
    }

    #[test]
    fn extend_then_read_returns_the_value() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 42);
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([42]));
    }

    #[test]
    fn several_values_on_one_key_accumulate() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(0), 2);
        dag.extend(Key(0), 1);
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2]));
    }

    #[test]
    fn is_empty_tracks_content() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        assert!(dag.is_empty(Key(0)));
        dag.extend(Key(0), 7);
        assert!(!dag.is_empty(Key(0)));
    }

    #[test]
    fn union_absorbs_source_set() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        dag.union(Key(0), Key(1));
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2]));
    }

    /// The other direction of [`union_leaves_source_untouched`]: the link is a
    /// live alias, so a value added to `src` after the union shows up in
    /// `dst`.
    #[test]
    fn union_aliases_the_source_set_rather_than_snapshotting_it() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        dag.union(Key(0), Key(1));
        dag.extend(Key(1), 99);
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2, 99]));
    }

    /// Repeating one pair must not grow the parents list: `for_each`'s `seen`
    /// hides the repetition in the answer, so only the link count shows it.
    #[test]
    fn repeating_one_union_pair_adds_one_link() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        for _ in 0..100 {
            dag.union(Key(0), Key(1));
        }
        let root = dag.roots[Key(0)].expand().expect("dst has a root");
        assert_eq!(dag.nodes[root].parents.as_slice(&dag.links).len(), 1);
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2]));
    }

    /// Repeating one value must not grow the parents list: `for_each`'s `seen`
    /// hides the repetition in the answer, so only the visit count shows it.
    #[test]
    fn repeating_one_extend_value_is_visited_once() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        for _ in 0..1000 {
            dag.extend(Key(0), 0xdead);
        }
        let mut visits = 0usize;
        dag.for_each(Key(0), |_| visits += 1);
        assert_eq!(visits, 1);
    }

    /// Interleaving two values must not defeat the guard either: an
    /// own-value-only check absorbs a leaf on every other call, so the walk
    /// cost grows with the CALL count while the answer stays at two values.
    #[test]
    fn alternating_extend_values_are_visited_once_each() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        for _ in 0..1000 {
            dag.extend(Key(0), 1);
            dag.extend(Key(0), 2);
        }
        let mut visits = 0usize;
        dag.for_each(Key(0), |_| visits += 1);
        assert_eq!(visits, 2);
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2]));
    }

    /// Interleaving two pairs must not defeat the guard either: a
    /// last-parent-only check pushes a link on every call, so the walk cost
    /// grows with the CALL count while the answer stays at three values.
    #[test]
    fn alternating_union_pairs_add_one_link_each() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        dag.extend(Key(2), 3);
        for _ in 0..100 {
            dag.union(Key(0), Key(1));
            dag.union(Key(0), Key(2));
        }
        let root = dag.roots[Key(0)].expand().expect("dst has a root");
        assert_eq!(dag.nodes[root].parents.as_slice(&dag.links).len(), 2);
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2, 3]));
    }

    /// A dense `seen` set grows a zeroed vector reaching the largest `UnionId`,
    /// so one call would cost the whole arena. The keys here are independent
    /// singletons, so each closure is one node: what this pins is `seen` being
    /// sized by the walk. The closure itself is the other half of the cost and
    /// is not measured here.
    #[test]
    fn for_each_cost_does_not_scale_with_the_arena() {
        fn sweep(n: u32) -> std::time::Duration {
            let mut dag: UnionDag<Key, u64> = UnionDag::new();
            for i in 0..n {
                dag.extend(Key(i), u64::from(i));
            }
            let start = std::time::Instant::now();
            let mut yielded = 0usize;
            for i in 0..n {
                dag.for_each(Key(i), |_| yielded += 1);
            }
            assert_eq!(yielded, n as usize);
            start.elapsed()
        }
        // Warm the allocator so the first sweep is not paying for it.
        sweep(4_000);
        let small = sweep(25_000);
        let large = sweep(200_000);
        // Linear would be 8x; quadratic 64x. A loose bound keeps this stable
        // on a loaded machine while still failing the dense-set shape.
        assert!(
            large.as_secs_f64() < small.as_secs_f64() * 24.0,
            "8x the keys cost {:.1}x the sweep ({small:?} -> {large:?})",
            large.as_secs_f64() / small.as_secs_f64(),
        );
    }

    #[test]
    fn union_leaves_source_untouched() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        dag.union(Key(0), Key(1));
        // Mutating dst must not leak back into src.
        dag.extend(Key(0), 3);
        assert_eq!(set_of(&dag, Key(1)), FxHashSet::from_iter([2]));
    }

    #[test]
    fn union_into_empty_key_gives_it_the_source_set() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(1), 9);
        dag.union(Key(0), Key(1)); // Key(0) had no set yet
        assert!(!dag.is_empty(Key(0)));
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([9]));
    }

    #[test]
    fn union_from_empty_source_is_a_noop() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.union(Key(0), Key(1)); // Key(1) empty
        assert!(dag.is_empty(Key(0)));
    }

    #[test]
    fn shared_subdag_is_collected_once_across_a_diamond() {
        // a and b both absorb c; d absorbs a and b. c is reached along two
        // paths but yields its value once.
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(2), 5); // c = Key(2)
        dag.extend(Key(0), 1); // a
        dag.extend(Key(1), 2); // b
        dag.union(Key(0), Key(2));
        dag.union(Key(1), Key(2));
        dag.union(Key(3), Key(0)); // d
        dag.union(Key(3), Key(1));

        let mut yielded = Vec::new();
        dag.for_each(Key(3), |v| yielded.push(v));
        yielded.sort_unstable();
        assert_eq!(yielded, vec![1, 2, 5], "5 appears once despite two paths");
    }

    #[test]
    fn mutual_union_forms_a_cycle_that_still_terminates() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        dag.union(Key(0), Key(1));
        dag.union(Key(1), Key(0)); // cycle: Key(0) <-> Key(1)
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1, 2]));
        assert_eq!(set_of(&dag, Key(1)), FxHashSet::from_iter([1, 2]));
    }

    #[test]
    fn self_union_terminates() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.union(Key(0), Key(0)); // self-loop
        assert_eq!(set_of(&dag, Key(0)), FxHashSet::from_iter([1]));
    }

    #[test]
    fn remap_relabels_keys_and_drops_culled() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(2), 3);
        dag.remap(|k| {
            if k == Key(2) {
                None
            } else {
                Some(Key(k.index() as u32 + 5))
            }
        });
        assert_eq!(set_of(&dag, Key(5)), FxHashSet::from_iter([1]));
        assert!(dag.is_empty(Key(7))); // old Key(2)+5, was culled
    }
}
