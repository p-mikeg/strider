//! A bidirectional interning map over a `cranelift-entity` key.

use core::hash::Hash;
use core::ops::Index;

use cranelift_entity::{EntityRef, PrimaryMap};
use rustc_hash::FxHashMap;

/// A bidirectional interning table: a forward `PrimaryMap<K, V>` (dense key
/// → value) paired with a reverse `V → K` index, kept consistent because the
/// only mutator is [`EntityInterner::intern`].
///
/// Interning a value returns its existing key when the value is already
/// present, otherwise it allocates the next dense key and records *both*
/// directions in lockstep — so the forward and reverse halves can never
/// drift. This is the recurring "dedup by value, dense id by allocation"
/// pattern: SSA variable tables (`Vn → VarId`), wide-constant interning
/// (`WideConstStorage → WideConstId`), and similar.
///
/// `V` must be `Clone` (one clone per genuinely-new value, to record both
/// directions) plus `Eq + Hash` (it keys the reverse map). For `Copy`
/// values the clone is free.
#[derive(Clone, Debug)]
pub struct EntityInterner<K: EntityRef, V: Clone + Eq + Hash> {
    /// Dense `K → V`, in allocation order.
    forward: PrimaryMap<K, V>,
    /// Reverse `V → K` index; an exact inverse of `forward`.
    reverse: FxHashMap<V, K>,
}

impl<K: EntityRef, V: Clone + Eq + Hash> EntityInterner<K, V> {
    /// Creates an empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `value`, returning its key. Idempotent: a value already
    /// present returns its existing key without allocating a new one.
    pub fn intern(&mut self, value: V) -> K {
        if let Some(&key) = self.reverse.get(&value) {
            return key;
        }
        let key = self.forward.push(value.clone());
        self.reverse.insert(value, key);
        key
    }

    /// Returns the value for `key`, or `None` when `key` is out of range.
    #[must_use]
    pub fn get(&self, key: K) -> Option<&V> {
        self.forward.get(key)
    }

    /// Returns the key for `value`, or `None` when it has not been interned.
    #[must_use]
    pub fn key_of(&self, value: &V) -> Option<K> {
        self.reverse.get(value).copied()
    }

    /// Returns whether `value` has been interned.
    #[must_use]
    pub fn contains(&self, value: &V) -> bool {
        self.reverse.contains_key(value)
    }

    /// Returns the number of interned values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.forward.len()
    }

    /// Returns whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    /// Iterates the keys in allocation order.
    pub fn keys(&self) -> impl Iterator<Item = K> + '_ {
        self.forward.keys()
    }

    /// Iterates the values in allocation (key) order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.forward.values()
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
        // forward
        assert_eq!(interner.get(k), Some(&42));
        assert_eq!(interner[k], 42);
        // reverse
        assert_eq!(interner.key_of(&42), Some(k));
        assert!(interner.contains(&42));
        // absent
        assert_eq!(interner.key_of(&99), None);
        assert!(!interner.contains(&99));
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
        let values: Vec<char> = interner.values().copied().collect();
        assert_eq!(values, vec!['p', 'q', 'r']);
    }

    #[test]
    fn default_is_empty() {
        let interner: EntityInterner<TestId, u8> = EntityInterner::default();
        assert!(interner.is_empty());
        assert_eq!(interner.len(), 0);
    }
}
