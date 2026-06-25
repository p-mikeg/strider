//! [`Inputs`] / [`InputCursor`] — input-slot navigation over a node's
//! intrusive input list.
//!
//! Ported from `strider-ir`'s `graph/iterators.rs`, generalized over the
//! node/value payloads `N`/`V`. Pure list navigation: it borrows the store's
//! arenas and yields `ValueId`s, never touching the payloads.

use core::ops::Index;
use core::slice;

use crate::cache::NodeCacheable;
use crate::graph::Graph;
use crate::ids::{NodeId, UseId, ValueId};

/// An iterable view over the input values of a node.
pub struct Inputs<'a, N, V, C: NodeCacheable<N, V>> {
    pub(crate) graph: &'a Graph<N, V, C>,
    pub(crate) use_list: &'a [UseId],
}

impl<N, V, C: NodeCacheable<N, V>> Clone for Inputs<'_, N, V, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N, V, C: NodeCacheable<N, V>> Copy for Inputs<'_, N, V, C> {}

impl<'a, N, V, C: NodeCacheable<N, V>> Inputs<'a, N, V, C> {
    /// The number of input slots.
    pub fn len(&self) -> usize {
        self.use_list.len()
    }

    /// Whether the node has no inputs.
    pub fn is_empty(&self) -> bool {
        self.use_list.is_empty()
    }

    /// The `ValueId` driving the `index`-th input slot, or `None` if out of
    /// bounds.
    pub fn get(&self, index: usize) -> Option<&ValueId> {
        Some(&self.graph.store.inputs[*self.use_list.get(index)?].value_id)
    }

    /// Iterator over each input slot's `ValueId` value.
    ///
    /// Same yield as `into_iter()` but borrows `self` so callers that only
    /// need a one-shot read don't have to move the `Inputs` value.
    pub fn iter(&self) -> impl Iterator<Item = ValueId> + Clone + 'a {
        let graph = self.graph;
        self.use_list
            .iter()
            .map(move |id| graph.store.inputs[*id].value_id)
    }
}

impl<'a, N, V, C: NodeCacheable<N, V>> IntoIterator for Inputs<'a, N, V, C> {
    type Item = ValueId;
    type IntoIter = InputIter<'a, N, V, C>;

    fn into_iter(self) -> Self::IntoIter {
        InputIter {
            graph: self.graph,
            iter: self.use_list.iter(),
        }
    }
}

impl<N, V, C: NodeCacheable<N, V>> Index<usize> for Inputs<'_, N, V, C> {
    type Output = ValueId;

    /// Index into the input slot list by position.
    ///
    /// # Panics
    ///
    /// Panics on out-of-bounds. Prefer the fallible `get(idx)` for
    /// opaque-arity nodes.
    fn index(&self, index: usize) -> &Self::Output {
        &self.graph.store.inputs[self.use_list[index]].value_id
    }
}

/// Concrete `IntoIter` for [`Inputs`].
pub struct InputIter<'a, N, V, C: NodeCacheable<N, V>> {
    pub(crate) graph: &'a Graph<N, V, C>,
    pub(crate) iter: slice::Iter<'a, UseId>,
}

impl<N, V, C: NodeCacheable<N, V>> Clone for InputIter<'_, N, V, C> {
    fn clone(&self) -> Self {
        InputIter {
            graph: self.graph,
            iter: self.iter.clone(),
        }
    }
}

impl<N, V, C: NodeCacheable<N, V>> Iterator for InputIter<'_, N, V, C> {
    type Item = ValueId;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.graph.store.inputs[*self.iter.next()?].value_id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

/// A cursor over the use-list of a single value, able to mutate the edge it
/// points at in place.
pub struct InputCursor<'a, N, V, C: NodeCacheable<N, V>> {
    pub(crate) graph: &'a mut Graph<N, V, C>,
    pub(crate) current: Option<UseId>,
}

impl<N, V, C: NodeCacheable<N, V>> InputCursor<'_, N, V, C> {
    /// The `(consumer, input_index)` the cursor currently points at, or `None`
    /// once the use-list is exhausted.
    pub fn current(&self) -> Option<(NodeId, u32)> {
        let current = &self.graph.store.inputs[self.current?];
        Some((current.node_id, current.input_index))
    }

    fn move_next(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        self.current = self.graph.store.inputs[current].next.expand();
    }

    /// Redirects the current input edge to `new_value` and advances.
    ///
    /// Returns `false` if the cursor was already past the end (no edge to
    /// replace), `true` otherwise.
    pub fn replace_current_with(&mut self, new_value: ValueId) -> bool {
        let Some(current) = self.current else {
            return false;
        };
        self.move_next();
        self.graph.update_input(current, new_value);
        true
    }
}
