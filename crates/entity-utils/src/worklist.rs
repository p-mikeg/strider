use alloc::collections::VecDeque;
use cranelift_entity::EntityRef;

use crate::set::DenseEntitySet;

/// A queue of unique [`EntityRef`] values for fixed-point iteration.
///
/// Each entity may be enqueued at most once at a time: if `entity` is already
/// in the queue, a second `enqueue` call is a no-op.  This prevents redundant
/// re-processing while still allowing an entity to be re-enqueued after it has
/// been dequeued.
#[derive(Clone)]
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

    /// Adds `entity` to the back of the queue.
    ///
    /// Has no effect if `entity` is already queued.
    pub fn enqueue(&mut self, entity: E) {
        if !self.workset.contains(entity) {
            self.workset.insert(entity);
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
        let worklist: VecDeque<_> = iter.into_iter().collect();
        let workset: DenseEntitySet<_> = worklist.iter().copied().collect();
        Self { worklist, workset }
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
}
