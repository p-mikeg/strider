use alloc::collections::VecDeque;
use cranelift_entity::EntityRef;

use super::set::DenseEntitySet;

/// A queue of unique [`EntityRef`] values for fixed-point iteration.
///
/// Each entity may be enqueued at most once at a time: if `entity` is already
/// in the queue, a second `enqueue` call is a no-op.  This prevents redundant
/// re-processing while still allowing an entity to be re-enqueued after it has
/// been dequeued.
#[derive(Clone, Debug)]
pub struct Worklist<E> {
    worklist: VecDeque<E>,
    workset: DenseEntitySet<E>,
}

impl<E: EntityRef> Worklist<E> {
    /// Creates an empty worklist.
    pub fn new() -> Self {
        Self {
            worklist: VecDeque::new(),
            workset: DenseEntitySet::new(),
        }
    }

    /// Returns `true` if the worklist contains no pending entities.
    pub fn is_empty(&self) -> bool {
        self.worklist.is_empty()
    }

    /// Number of entities currently queued.
    pub fn len(&self) -> usize {
        self.worklist.len()
    }

    /// Returns `true` if `entity` is currently queued.
    pub fn contains(&self, entity: E) -> bool {
        self.workset.contains(entity)
    }

    /// Removes every queued entity.
    pub fn clear(&mut self) {
        self.worklist.clear();
        self.workset.clear();
    }

    /// Adds `entity` to the back of the queue.
    ///
    /// Has no effect if `entity` is already queued.
    pub fn enqueue(&mut self, entity: E) {
        if self.workset.insert(entity) {
            self.worklist.push_back(entity);
        }
    }

    /// Removes and returns the next entity from the front of the queue, or
    /// `None` if the queue is empty.
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
        // Before the fix this would fail: enqueue never inserts into the
        // workset, so both pushes land in the deque and the second dequeue
        // returns Some instead of None.
        assert_eq!(wl.dequeue(), Some(Id(7)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn from_iter_dedups_duplicates() {
        let mut wl: Worklist<Id> =
            [Id(1), Id(2), Id(1), Id(3), Id(2)].into_iter().collect();
        let mut got = Vec::new();
        while let Some(e) = wl.dequeue() {
            got.push(e);
        }
        // Order of first occurrence preserved; duplicates dropped.
        assert_eq!(got, vec![Id(1), Id(2), Id(3)]);
    }

    #[test]
    fn new_and_default_are_empty() {
        let wl: Worklist<Id> = Worklist::new();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);

        let wl: Worklist<Id> = Worklist::default();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);
    }

    #[test]
    fn enqueue_dequeue_roundtrip() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(0));
        assert!(!wl.is_empty());
        assert_eq!(wl.len(), 1);
        assert!(wl.contains(Id(0)));

        assert_eq!(wl.dequeue(), Some(Id(0)));
        assert!(wl.is_empty());
        assert!(!wl.contains(Id(0)));
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
        assert_eq!(wl.len(), 2);
        assert_eq!(wl.dequeue(), Some(Id(1)));
        assert_eq!(wl.dequeue(), Some(Id(2)));
        assert_eq!(wl.dequeue(), None);
    }

    #[test]
    fn clear_empties_both_queue_and_set() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.extend([Id(1), Id(2), Id(3)]);
        wl.clear();
        assert!(wl.is_empty());
        assert_eq!(wl.len(), 0);
        assert!(!wl.contains(Id(1)));

        // After clear, re-enqueue still works (workset must really be empty,
        // not just have stale entries).
        wl.enqueue(Id(1));
        assert_eq!(wl.dequeue(), Some(Id(1)));
    }

    #[test]
    fn contains_only_while_queued() {
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(5));
        assert!(wl.contains(Id(5)));
        let _ = wl.dequeue();
        assert!(!wl.contains(Id(5)));
    }

    #[test]
    fn debug_format_pins_derive() {
        // `Worklist` derives Debug; this is a regression pin so the derive
        // can't be silently removed.  We don't assert a specific format string.
        let mut wl: Worklist<Id> = Worklist::new();
        wl.enqueue(Id(1));
        let _ = format!("{wl:?}");
    }

    /// `enqueue` deduplicates at 10k-item scale.  Pins the
    /// single-pass `if workset.insert(e) { push }` shape —
    /// re-enqueueing the same id never duplicates the queue.
    #[test]
    fn enqueue_dedup_at_ten_thousand_scale() {
        let n: u32 = 10_000;
        let mut wl: Worklist<Id> = Worklist::new();
        for i in 0..n {
            wl.enqueue(Id(i));
        }
        assert_eq!(wl.len(), n as usize);
        for i in 0..n {
            wl.enqueue(Id(i));
        }
        assert_eq!(wl.len(), n as usize, "no duplicates after re-enqueue");
        let mut count = 0usize;
        while wl.dequeue().is_some() {
            count += 1;
        }
        assert_eq!(count, n as usize);
        assert!(wl.is_empty());
    }
}
