use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{EntityList, EntityRef, ListPool, PrimaryMap, SecondaryMap, entity_impl};

use crate::DenseEntitySet;

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
pub struct UnionDag<N: EntityRef, V: Copy> {
    /// `NONE` means the key has no set yet.
    roots: SecondaryMap<N, PackedOption<UnionId>>,
    nodes: PrimaryMap<UnionId, Node<V>>,
    links: ListPool<UnionId>,
}

impl<N: EntityRef, V: Copy> Default for UnionDag<N, V> {
    fn default() -> Self {
        Self {
            roots: SecondaryMap::new(),
            nodes: PrimaryMap::new(),
            links: ListPool::new(),
        }
    }
}

impl<N: EntityRef, V: Copy> UnionDag<N, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// O(1) amortised: the first value fills `n`'s own node, later ones become
    /// absorbed leaves.
    pub fn extend(&mut self, n: N, v: V) {
        match self.roots[n].expand() {
            None => {
                let id = self.alloc(Some(v));
                self.roots[n] = id.into();
            }
            Some(root) if self.nodes[root].own.is_none() => self.nodes[root].own = Some(v),
            Some(root) => {
                let leaf = self.alloc(Some(v));
                self.nodes[root].parents.push(leaf, &mut self.links);
            }
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
        // Repeating one `(dst, src)` pair would otherwise grow `dst`'s parents
        // without bound and turn `for_each` linear in the number of `union`
        // calls: its `seen` set hides the repetition in the ANSWER, not in the
        // COST. Checking the last entry catches the repeated pair; an
        // interleaved re-union costs one redundant link.
        let last = self.nodes[dst_root]
            .parents
            .as_slice(&self.links)
            .last()
            .copied();
        if last == Some(src_root) {
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
    pub fn for_each(&self, n: N, mut f: impl FnMut(V)) {
        let Some(root) = self.roots[n].expand() else {
            return;
        };
        let mut seen: DenseEntitySet<UnionId> = DenseEntitySet::new();
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
    /// DAG arena is untouched.
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
