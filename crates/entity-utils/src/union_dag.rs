use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{EntityList, EntityRef, ListPool, PrimaryMap, SecondaryMap, entity_impl};
use rustc_hash::FxHashSet;
use std::hash::Hash;

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

/// Per-key value sets in which [`Self::union`] makes one key's set a live
/// subset of another's.
#[derive(Clone, Debug)]
pub struct UnionDag<N: EntityRef, V: Copy + Eq + Hash> {
    /// `NONE` means the key has no set yet.
    roots: SecondaryMap<N, PackedOption<UnionId>>,
    nodes: PrimaryMap<UnionId, Node<V>>,
    links: ListPool<UnionId>,
    /// The `(dst, src)` pairs already linked.
    linked: FxHashSet<(UnionId, UnionId)>,
    /// The `(root, value)` pairs already extended.
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

    /// O(1) amortised. Re-adding a value held on `n`'s own root is a no-op;
    /// one reached only through a union is added again, and
    /// [`Self::for_each`] then yields it twice.
    pub fn extend(&mut self, n: N, v: V) {
        let root = self.ensure(n);
        // As `linked` in `union`: keeps `parents` from growing per call.
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

    /// O(1) amortised: links `src`'s root under `dst`, a live alias: values
    /// `src` gains later are `dst`'s too, not the reverse. A no-op while `src`
    /// is empty, so values it gains after such a call are not.
    pub fn union(&mut self, dst: N, src: N) {
        let Some(src_root) = self.roots[src].expand() else {
            return;
        };
        let dst_root = self.ensure(dst);
        // One `parents` entry per distinct link, however often re-linked, so
        // `for_each` cost does not grow with the call count.
        if !self.linked.insert((dst_root, src_root)) {
            return;
        }
        self.nodes[dst_root].parents.push(src_root, &mut self.links);
    }

    pub fn is_empty(&self, n: N) -> bool {
        self.roots[n].is_none()
    }

    /// Visits every value reachable from `n`'s set, cycle-safe. Dedup is per
    /// node: one value held by two nodes is yielded twice. Costs the closure
    /// reached from `n`, so sweeping every key of a union chain is quadratic.
    pub fn for_each(&self, n: N, mut f: impl FnMut(V)) {
        let Some(root) = self.roots[n].expand() else {
            return;
        };
        // Sized by what the walk visits, not by the arena.
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

    /// Relabels keys after the `N` space is compacted; `f` must be injective.
    /// `None` culls a key's entry point but not its DAG node, so a key that
    /// unioned from it still reaches its values. The arena is untouched and a
    /// `UnionId` never reused, so `held` / `linked` need no pruning.
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

    /// Random extends and unions against a model that recomputes each set
    /// from the live links.
    #[test]
    fn random_unions_match_a_live_alias_model() {
        const KEYS: usize = 6;
        let runs = if cfg!(debug_assertions) {
            2_000
        } else {
            50_000
        };
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        for run in 0..runs {
            let mut dag: UnionDag<Key, u64> = UnionDag::new();
            let mut rooted = [false; KEYS];
            let mut own: [FxHashSet<u64>; KEYS] = Default::default();
            let mut links: [Vec<usize>; KEYS] = Default::default();
            for _ in 0..next(24) {
                let (a, b) = (next(KEYS as u64) as usize, next(KEYS as u64) as usize);
                if next(2) == 0 {
                    let v = next(5);
                    dag.extend(Key(a as u32), v);
                    rooted[a] = true;
                    own[a].insert(v);
                } else {
                    dag.union(Key(a as u32), Key(b as u32));
                    if rooted[b] {
                        rooted[a] = true;
                        links[a].push(b);
                    }
                }
            }
            for (k, &is_rooted) in rooted.iter().enumerate() {
                let mut want = FxHashSet::default();
                let mut seen = [false; KEYS];
                let mut stack = vec![k];
                while let Some(x) = stack.pop() {
                    if std::mem::replace(&mut seen[x], true) {
                        continue;
                    }
                    want.extend(own[x].iter().copied());
                    stack.extend(&links[x]);
                }
                assert_eq!(
                    dag.is_empty(Key(k as u32)),
                    !is_rooted,
                    "run {run}: key {k}"
                );
                assert_eq!(set_of(&dag, Key(k as u32)), want, "run {run}: key {k}");
            }
        }
    }
}
