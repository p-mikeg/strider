use alloc::collections::VecDeque;
use cranelift_entity::EntityRef;

use super::set::DenseEntitySet;

/// FIFO queue for fixed-point iteration, holding each entity at most once:
/// enqueueing one already queued is a no-op, but an entity may be re-enqueued
/// once dequeued.
#[derive(Clone, Debug)]
pub struct Worklist<E> {
    worklist: VecDeque<E>,
    workset: DenseEntitySet<E>,
}

impl<E: EntityRef> Worklist<E> {
    pub fn new() -> Self {
        Self {
            worklist: VecDeque::new(),
            workset: DenseEntitySet::new(),
        }
    }

    pub fn enqueue(&mut self, entity: E) {
        if self.workset.insert(entity) {
            self.worklist.push_back(entity);
        }
    }

    pub fn dequeue(&mut self) -> Option<E> {
        let entity = self.worklist.pop_front()?;
        self.workset.remove(entity);
        Some(entity)
    }
}

impl<E: EntityRef> Default for Worklist<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: EntityRef> FromIterator<E> for Worklist<E> {
    fn from_iter<T: IntoIterator<Item = E>>(iter: T) -> Self {
        let mut wl = Self::new();
        wl.extend(iter);
        wl
    }
}

impl<E: EntityRef> Extend<E> for Worklist<E> {
    fn extend<T: IntoIterator<Item = E>>(&mut self, iter: T) {
        for entity in iter {
            self.enqueue(entity);
        }
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
    fn enqueue_dedups_while_queued() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(7));
        wl.enqueue(Id(7));
        assert_eq!(wl.dequeue(), Some(Id(7)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn from_iter_dedups_duplicates() {
        let mut wl: Worklist<Id> = [Id(1), Id(2), Id(1), Id(3), Id(2)].into_iter().collect();
        let mut got = Vec::new();
        while let Some(e) = wl.dequeue() {
            got.push(e);
        }
        // First-occurrence order preserved.
        assert_eq!(got, vec![Id(1), Id(2), Id(3)]);
    }

    #[test]
    fn new_and_default_are_empty() {
        let mut wl: Worklist<Id> = Worklist::new();
        assert_eq!(wl.dequeue(), None);

        let mut wl: Worklist<Id> = Worklist::default();
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn enqueue_dequeue_roundtrip() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(0));
        assert!(wl.workset.contains(Id(0)));

        assert_eq!(wl.dequeue(), Some(Id(0)));
        assert!(!wl.workset.contains(Id(0)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn fifo_order_across_distinct_entities() {
        let mut wl: Worklist<Id> = Worklist::new();
        for i in 0..5 {
            wl.enqueue(Id(i));
        }
        let mut got = Vec::new();
        while let Some(e) = wl.dequeue() {
            got.push(e);
        }
        assert_eq!(got, (0..5).map(Id).collect::<Vec<_>>());
    }

    #[test]
    fn re_enqueue_after_dequeue_is_allowed() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(42));
        assert_eq!(wl.dequeue(), Some(Id(42)));

        wl.enqueue(Id(42));
        assert_eq!(wl.dequeue(), Some(Id(42)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn extend_dedups() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.extend([Id(1), Id(2), Id(1)]);
        assert_eq!(wl.dequeue(), Some(Id(1)));
        assert_eq!(wl.dequeue(), Some(Id(2)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn contains_only_while_queued() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(5));
        assert!(wl.workset.contains(Id(5)));
        let _ = wl.dequeue();
        assert!(!wl.workset.contains(Id(5)));
    }

    #[test]
    fn debug_format_pins_derive() {
        // Pins the `Debug` derive against silent removal.
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(1));
        let _ = format!("{wl:?}");
    }

    /// Pins the single-pass `if workset.insert(e) { push }` shape at scale.
    #[test]
    fn enqueue_dedup_at_ten_thousand_scale() {
        let n: u32 = 10_000;
        let mut wl: Worklist<Id> = Worklist::new();
        for i in 0..n {
            wl.enqueue(Id(i));
        }
        for i in 0..n {
            wl.enqueue(Id(i));
        }
        let mut count = 0usize;
        while wl.dequeue().is_some() {
            count += 1;
        }
        assert_eq!(count, n as usize, "no duplicates after re-enqueue");
        assert_eq!(wl.dequeue(), None);
    }
}
