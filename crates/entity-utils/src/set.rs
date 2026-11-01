use core::fmt;

use cranelift_entity::{EntityRef, EntitySet, SetIter};

/// A bitset of entities. O(1) `contains` and `insert`; `is_empty` and `iter`
/// are O(capacity / 64), and so is `remove` of the current maximum, where
/// capacity follows the largest index ever inserted and survives `clear`
/// and `remove`.
#[derive(Clone)]
pub struct DenseEntitySet<E: EntityRef>(EntitySet<E>);

impl<E: EntityRef> DenseEntitySet<E> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, e: E) -> bool {
        self.0.contains(e)
    }

    /// `true` if `e` was not already a member.
    pub fn insert(&mut self, e: E) -> bool {
        self.0.insert(e)
    }

    /// `true` if `e` was a member.
    pub fn remove(&mut self, e: E) -> bool {
        self.0.remove(e)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// In ascending index order.
    pub fn iter(&self) -> SetIter<'_, E> {
        self.0.iter()
    }
}

impl<E: EntityRef> Default for DenseEntitySet<E> {
    fn default() -> Self {
        Self(EntitySet::new())
    }
}

impl<E: EntityRef> Extend<E> for DenseEntitySet<E> {
    fn extend<T: IntoIterator<Item = E>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

impl<E: EntityRef + fmt::Debug> fmt::Debug for DenseEntitySet<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<'a, E: EntityRef> IntoIterator for &'a DenseEntitySet<E> {
    type Item = E;
    type IntoIter = SetIter<'a, E>;

    fn into_iter(self) -> SetIter<'a, E> {
        self.iter()
    }
}

impl<E: EntityRef> FromIterator<E> for DenseEntitySet<E> {
    fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
        let mut set = Self::new();
        set.extend(iter);
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
    fn insert_contains_remove() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(!s.contains(Id(3)));
        s.insert(Id(3));
        assert!(s.contains(Id(3)));
        s.remove(Id(3));
        assert!(!s.contains(Id(3)));
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
    fn from_iter_dedups() {
        let s: DenseEntitySet<Id> = [Id(2), Id(1), Id(2), Id(3)].into_iter().collect();
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
    fn insert_returns_true_on_first_insert_false_on_repeat() {
        let mut s: DenseEntitySet<Id> = DenseEntitySet::new();
        assert!(
            s.insert(Id(42)),
            "first insert must report 'newly inserted'"
        );
        assert!(
            !s.insert(Id(42)),
            "repeat insert must report 'already present'"
        );
        assert!(s.insert(Id(43)), "different entity is newly inserted");
    }

    #[test]
    fn debug_lists_members_only() {
        let s: DenseEntitySet<Id> = core::iter::once(Id(2)).collect();
        assert_eq!(format!("{s:?}"), "{Id(2)}");
    }
}
