//! A DAG of deferred unions over per-node values, keyed by an external
//! [`EntityRef`].
//!
//! Each external key `N` maps to at most one DAG node holding an optional
//! value `V`; a node absorbs others by linking (not copying), so
//! [`union`](UnionDag::union) is O(1) — a single [`EntityList`] push. A key's
//! full value set is materialised only on demand by walking its ancestors
//! ([`for_each`](UnionDag::for_each)), deduplicating shared sub-DAGs.
//!
//! This is the "cheaply accumulate overlapping sets, read them rarely"
//! pattern: absorb is on the hot path, materialise is off it.

use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{entity_impl, EntityList, EntityRef, ListPool, PrimaryMap, SecondaryMap};

use crate::DenseEntitySet;

/// Internal DAG-node id. Never exposed: callers address the DAG by their own
/// external key `N`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct UnionId(u32);
entity_impl!(UnionId);

#[derive(Clone, Debug)]
struct Node<V> {
    /// The value this node contributes to its set, if any. A pure join node
    /// (created by [`UnionDag::union`] on a key that had no value yet) carries
    /// `None`.
    own: Option<V>,
    /// Absorbed nodes whose sets are unioned into this one.
    parents: EntityList<UnionId>,
}

/// A DAG of deferred unions over values `V`, addressed by external key `N`.
#[derive(Clone, Debug)]
pub struct UnionDag<N: EntityRef, V: Copy> {
    /// External key → its DAG root. `NONE` means the key has no set yet.
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
    /// Creates an empty DAG.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one value to `n`'s set. The first value fills `n`'s node; later
    /// values become absorbed leaf nodes. O(1) amortised.
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

    /// Makes `dst`'s set absorb `src`'s — O(1). A no-op when `src` is empty.
    pub fn union(&mut self, dst: N, src: N) {
        let Some(src_root) = self.roots[src].expand() else {
            return;
        };
        let dst_root = self.ensure(dst);
        self.nodes[dst_root].parents.push(src_root, &mut self.links);
    }

    /// Whether `n` has no set at all. O(1).
    pub fn is_empty(&self, n: N) -> bool {
        self.roots[n].is_none()
    }

    /// Calls `f` with every value reachable from `n`'s set, each contributing
    /// DAG node visited exactly once (shared sub-DAGs are not re-walked; a
    /// value held by two distinct nodes is still yielded twice — the caller's
    /// collection deduplicates values).  Cycle-safe: absorbing in both
    /// directions is permitted and still terminates.
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

    /// Remaps external keys after a compaction of the `N` space. `f` returns
    /// the key's new id, or `None` if it was culled (its set is dropped). The
    /// DAG arena is untouched — only the key→root map is rebuilt.
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

    /// Returns `n`'s root, creating a fresh valueless join node if it has none.
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

    /// Allocates a fresh DAG node carrying `own` and no parents.
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
        dag.extend(Key(0), 1); // dup value, same key
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

    #[test]
    fn union_leaves_source_untouched() {
        let mut dag: UnionDag<Key, u64> = UnionDag::new();
        dag.extend(Key(0), 1);
        dag.extend(Key(1), 2);
        dag.union(Key(0), Key(1));
        // Mutating dst after the union must not leak back into src.
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
        // c holds {5}; a and b both absorb c; d absorbs a and b. d's set is
        // {5} — c reached along two paths, its value yielded once (node-dedup).
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
        // Key(0) -> Key(5); Key(2) culled.
        dag.remap(|k| if k == Key(2) { None } else { Some(Key(k.index() as u32 + 5)) });
        assert_eq!(set_of(&dag, Key(5)), FxHashSet::from_iter([1]));
        assert!(dag.is_empty(Key(7))); // old Key(2)+5, was culled
    }
}
