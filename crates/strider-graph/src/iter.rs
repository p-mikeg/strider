use core::ops::Index;
use core::slice;

use crate::cache::NodeCacheable;
use crate::graph::Graph;
use crate::ids::{UseId, ValueId};

#[cfg(feature = "edge-read-count")]
std::thread_local! {
    static EDGE_READS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

/// `Inputs` / `InputIter` reads, `Graph::nth_input` calls and
/// `Graph::value_uses` steps on this thread so far.
#[cfg(feature = "edge-read-count")]
pub fn edge_reads() -> u64 {
    EDGE_READS.with(core::cell::Cell::get)
}

#[inline]
pub(crate) fn count_edge_read() {
    #[cfg(feature = "edge-read-count")]
    EDGE_READS.with(|n| n.set(n.get() + 1));
}

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
    pub fn len(&self) -> usize {
        self.use_list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.use_list.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&ValueId> {
        let id = *self.use_list.get(index)?;
        count_edge_read();
        Some(&self.graph.store.inputs[id].value_id)
    }

    /// Same yield as `into_iter()`, borrowing instead of consuming.
    pub fn iter(&self) -> impl Iterator<Item = ValueId> + Clone + 'a {
        let graph = self.graph;
        self.use_list.iter().map(move |id| {
            count_edge_read();
            graph.store.inputs[*id].value_id
        })
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

    /// # Panics
    ///
    /// On out-of-bounds.
    fn index(&self, index: usize) -> &Self::Output {
        count_edge_read();
        &self.graph.store.inputs[self.use_list[index]].value_id
    }
}

pub struct InputIter<'a, N, V, C: NodeCacheable<N, V>> {
    pub(crate) graph: &'a Graph<N, V, C>,
    iter: slice::Iter<'a, UseId>,
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
        let id = *self.iter.next()?;
        count_edge_read();
        Some(self.graph.store.inputs[id].value_id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }

    /// The default walks `n` edges one at a time; the use-list is a slice.
    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        let id = *self.iter.nth(n)?;
        count_edge_read();
        Some(self.graph.store.inputs[id].value_id)
    }
}
