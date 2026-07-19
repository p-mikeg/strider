use core::hash::Hash;
use core::ops::Index;

use cranelift_entity::{EntityRef, PrimaryMap};
use rustc_hash::FxHashMap;

/// Dedup by value, dense id by allocation.
///
/// The forward `PrimaryMap` and the reverse index can never drift: [`intern`]
/// is the only mutator and writes both halves in lockstep.
///
/// `V: Clone` costs one clone per genuinely-new value (free when `Copy`);
/// `Eq + Hash` keys the reverse map.
///
/// [`intern`]: EntityInterner::intern
#[derive(Clone, Debug)]
pub struct EntityInterner<K: EntityRef, V: Clone + Eq + Hash> {
    forward: PrimaryMap<K, V>,
    reverse: FxHashMap<V, K>,
}

impl<K: EntityRef, V: Clone + Eq + Hash> EntityInterner<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Idempotent: an already-present value returns its existing key.
    pub fn intern(&mut self, value: V) -> K {
        if let Some(&key) = self.reverse.get(&value) {
            return key;
        }
        let key = self.forward.push(value.clone());
        self.reverse.insert(value, key);
        key
    }

    pub fn get(&self, key: K) -> Option<&V> {
        self.forward.get(key)
    }

    pub fn key_of(&self, value: &V) -> Option<K> {
        self.reverse.get(value).copied()
    }

    pub fn len(&self) -> usize {
        self.forward.len()
    }

    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// Allocation order.
    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.forward.keys()
    }

    /// Allocation order, so a caller can index it by a key's `.index()`.
    /// Borrows the forward map's element vec: O(1), no allocation.
    pub fn values_as_slice(&self) -> &[V] {
        self.forward.values().as_slice()
    }
}

impl<K: EntityRef, V: Clone + Eq + Hash> Default for EntityInterner<K, V> {
    fn default() -> Self {
        Self {
            forward: PrimaryMap::new(),
            reverse: FxHashMap::default(),
        }
    }
}

impl<K: EntityRef, V: Clone + Eq + Hash> Index<K> for EntityInterner<K, V> {
    type Output = V;

    /// Panics on a key this interner did not produce; use
    /// [`get`](EntityInterner::get) when provenance is uncertain.
    #[track_caller]
    fn index(&self, key: K) -> &V {
        &self.forward[key]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    cranelift_entity::entity_impl!(TestId);
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    struct TestId(u32);

    #[test]
    fn intern_allocates_dense_keys_in_order() {
        let mut interner: EntityInterner<TestId, &'static str> = EntityInterner::new();
        let a = interner.intern("a");
        let b = interner.intern("b");
        let c = interner.intern("c");
        assert_eq!(a.index(), 0);
        assert_eq!(b.index(), 1);
        assert_eq!(c.index(), 2);
        assert_eq!(interner.len(), 3);
    }

    #[test]
    fn intern_is_idempotent() {
        let mut interner: EntityInterner<TestId, &'static str> = EntityInterner::new();
        let first = interner.intern("x");
        let again = interner.intern("x");
        assert_eq!(first, again, "re-interning a value returns the same key");
        assert_eq!(interner.len(), 1, "no new key is allocated for a duplicate");
    }

    #[test]
    fn forward_and_reverse_agree() {
        let mut interner: EntityInterner<TestId, u64> = EntityInterner::new();
        let k = interner.intern(42);
        assert_eq!(interner.get(k), Some(&42));
        assert_eq!(interner[k], 42);
        assert_eq!(interner.key_of(&42), Some(k));
        assert_eq!(interner.key_of(&99), None);
        assert_eq!(interner.get(TestId(7)), None);
    }

    #[test]
    fn keys_and_values_iterate_in_allocation_order() {
        let mut interner: EntityInterner<TestId, char> = EntityInterner::new();
        for c in ['p', 'q', 'r'] {
            interner.intern(c);
        }
        let keys: Vec<usize> = interner.keys().map(|k| k.index()).collect();
        assert_eq!(keys, vec![0, 1, 2]);
        let values: Vec<char> = interner.values_as_slice().to_vec();
        assert_eq!(values, vec!['p', 'q', 'r']);
    }

    #[test]
    fn intern_reverse_collision_distinct_keys() {
        // Degenerate `Hash` forces every value into one reverse-map bucket;
        // `Eq` must still keep distinct values on distinct keys.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        struct Collide(u32);
        impl core::hash::Hash for Collide {
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                0u8.hash(state);
            }
        }

        let mut interner: EntityInterner<TestId, Collide> = EntityInterner::new();
        let a = interner.intern(Collide(1));
        let b = interner.intern(Collide(2));
        assert_ne!(a, b, "colliding-hash but distinct values get distinct keys");
        assert_eq!(interner.len(), 2);
        assert_eq!(interner.get(a), Some(&Collide(1)));
        assert_eq!(interner.get(b), Some(&Collide(2)));
        assert_eq!(interner.key_of(&Collide(1)), Some(a));
        assert_eq!(interner.key_of(&Collide(2)), Some(b));
        assert_eq!(interner.intern(Collide(1)), a);
        assert_eq!(interner.intern(Collide(2)), b);
    }

    #[test]
    fn default_is_empty() {
        let interner: EntityInterner<TestId, u8> = EntityInterner::default();
        assert!(interner.is_empty());
        assert_eq!(interner.len(), 0);
    }
}
