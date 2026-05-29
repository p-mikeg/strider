use core::marker::PhantomData;

use cranelift_bitset::CompoundBitSet;
use cranelift_entity::EntityRef;

/// A dense bit-set of [`cranelift_entity::EntityRef`] values.
///
/// Backed by a [`cranelift_bitset::CompoundBitSet`], this provides O(1)
/// membership tests and updates using the entity's integer index.  Suitable
/// as a visited-set in graph traversals over dense id spaces.
#[derive(Clone, Debug)]
pub struct DenseEntitySet<E> {
    bitset: CompoundBitSet,
    _marker: PhantomData<E>,
}

impl<E: EntityRef> DenseEntitySet<E> {
    /// Creates an empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty set pre-allocated for at least `capacity` entities.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bitset: CompoundBitSet::with_capacity(capacity),
            _marker: PhantomData,
        }
    }

    /// Clears all entries from the set.
    pub fn clear(&mut self) {
        self.bitset.clear();
    }

    /// Returns the number of entities currently in the set.
    ///
    /// Runs in O(max_index / 64) — it sums the population counts of the
    /// backing words rather than reading a cached length, so this is not
    /// O(1).
    #[must_use]
    pub fn len(&self) -> usize {
        self.bitset.len()
    }

    /// Returns `true` if the set contains no entities.
    ///
    /// Runs in O(max_index / 64) for the same reason as
    /// [`DenseEntitySet::len`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bitset.is_empty()
    }

    /// Returns `true` if `entity` is a member of the set.
    #[must_use]
    pub fn contains(&self, entity: E) -> bool {
        self.bitset.contains(entity.index())
    }

    /// Inserts `entity` into the set.  Returns `true` if `entity` was
    /// newly inserted, `false` if it was already present — matching
    /// `std::collections::HashSet::insert`'s contract so callers can
    /// switch implementations without changing the call shape.
    ///
    /// Single-pass: delegates directly to
    /// `cranelift_bitset::CompoundBitSet::insert`, which itself
    /// returns `bool` with the same "was newly inserted" semantics.
    /// Hot graph-traversal paths get one bitset access per insert.
    pub fn insert(&mut self, entity: E) -> bool {
        self.bitset.insert(entity.index())
    }

    /// Removes `entity` from the set.
    pub fn remove(&mut self, entity: E) {
        self.bitset.remove(entity.index());
    }

    /// Returns an iterator over all entities currently in the set, in
    /// ascending entity-index order.
    ///
    /// Iterating fully runs in O(max_index / 64 + len): it scans the backing
    /// words (skipping empty ones) and yields one entity per set bit.
    #[must_use]
    pub fn iter(&self) -> Iter<'_, E> {
        Iter::<E> {
            inner: self.bitset.iter(),
            _marker: PhantomData,
        }
    }
}

/// Iterator over a [`DenseEntitySet`] in ascending entity-index order.
///
/// Returned by [`DenseEntitySet::iter`] and `<&DenseEntitySet>::into_iter`.
pub struct Iter<'a, E> {
    inner: cranelift_bitset::compound::Iter<'a>,
    _marker: PhantomData<E>,
}

impl<E: EntityRef> Iterator for Iter<'_, E> {
    type Item = E;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(E::new)
    }
}

// `cranelift_bitset::compound::Iter` keeps yielding `None` once exhausted,
// so the wrapper is naturally fused.
impl<E: EntityRef> core::iter::FusedIterator for Iter<'_, E> {}

impl<'a, E: EntityRef> IntoIterator for &'a DenseEntitySet<E> {
    type Item = E;
    type IntoIter = Iter<'a, E>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<E> Default for DenseEntitySet<E> {
    fn default() -> Self {
        Self {
            bitset: CompoundBitSet::default(),
            _marker: PhantomData,
        }
    }
}

impl<E: EntityRef> FromIterator<E> for DenseEntitySet<E> {
    fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let mut set = Self::with_capacity(upper.unwrap_or(lower));
        for entity in iter {
            set.insert(entity);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::entity_impl;

    #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
    struct Id(u32);
    entity_impl!(Id);

    #[test]
    fn new_and_default_are_empty() {
        let s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(!s.contains(Id(0)));
        assert!(s.iter().next().is_none());

        let s: DenseEntitySet<Id> = DenseEntitySet::default();
        assert!(!s.contains(Id(0)));
    }

    #[test]
    fn with_capacity_zero_is_valid() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::with_capacity(0);
        assert!(s.iter().next().is_none());
        s.insert(Id(0));
        assert!(s.contains(Id(0)));
    }

    #[test]
    fn insert_contains_remove() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(!s.contains(Id(3)));
        s.insert(Id(3));
        assert!(s.contains(Id(3)));
        s.remove(Id(3));
        assert!(!s.contains(Id(3)));
    }

    #[test]
    fn double_insert_is_idempotent() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        s.insert(Id(7));
        s.insert(Id(7));
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Id(7)]);
    }

    #[test]
    fn iter_yields_in_ascending_index_order() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        for &i in &[5u32, 1, 9, 2, 1] {
            s.insert(Id(i));
        }
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Id(1), Id(2), Id(5), Id(9)]);
    }

    #[test]
    fn clear_removes_everything() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        s.insert(Id(0));
        s.insert(Id(100));
        s.clear();
        assert!(!s.contains(Id(0)));
        assert!(!s.contains(Id(100)));
        assert!(s.iter().next().is_none());
        // Verify re-insert after clear works (bitset is fully reset).
        s.insert(Id(0));
        assert!(s.contains(Id(0)));
    }

    #[test]
    fn from_iter_dedups() {
        let s: DenseEntitySet<Id> =
            [Id(2), Id(1), Id(2), Id(3)].into_iter().collect();
        let collected: Vec<_> = s.iter().collect();
        assert_eq!(collected, vec![Id(1), Id(2), Id(3)]);
    }

    #[test]
    fn from_iter_empty() {
        let s: DenseEntitySet<Id> = core::iter::empty().collect();
        assert!(s.iter().next().is_none());
    }

    #[test]
    fn remove_unmembered_is_noop() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        s.insert(Id(1));
        s.remove(Id(99));
        assert!(s.contains(Id(1)));
        assert!(!s.contains(Id(99)));
    }

    #[test]
    fn len_and_is_empty_track_membership() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        s.insert(Id(1));
        s.insert(Id(2));
        assert_eq!(s.len(), 2);
        assert!(!s.is_empty());
        s.insert(Id(1)); // idempotent
        assert_eq!(s.len(), 2);
        s.remove(Id(1));
        assert_eq!(s.len(), 1);
        s.clear();
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn into_iter_for_ref_yields_same_as_iter() {
        let s: DenseEntitySet<Id> = [Id(3), Id(1), Id(4)].into_iter().collect();
        let by_iter: Vec<_> = s.iter().collect();
        let by_for: Vec<_> = (&s).into_iter().collect();
        assert_eq!(by_iter, by_for);
        let mut by_for_sugar = Vec::new();
        for id in &s {
            by_for_sugar.push(id);
        }
        assert_eq!(by_iter, by_for_sugar);
    }

    #[test]
    fn iter_is_fused() {
        fn assert_fused<I: core::iter::FusedIterator>(_: &I) {}
        let s: DenseEntitySet<Id> = core::iter::once(Id(1)).collect();
        let it = s.iter();
        assert_fused(&it);
    }

    #[test]
    fn insert_returns_true_on_first_insert_false_on_repeat() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(s.insert(Id(42)), "first insert must report 'newly inserted'");
        assert!(!s.insert(Id(42)), "repeat insert must report 'already present'");
        assert!(s.insert(Id(43)), "different entity is newly inserted");
    }
}
