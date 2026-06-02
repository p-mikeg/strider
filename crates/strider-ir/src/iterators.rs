use core::{ops::Index, slice};

use anyhow::anyhow;

use super::graph::Graph;
use super::node::{NodeId, UseId, ValueId};

#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub(crate) graph: &'a Graph,
    pub(crate) use_list: &'a [UseId],
}

impl<'a> Inputs<'a> {
    #[must_use]
    pub fn len(&self) -> usize {
        self.use_list.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.use_list.is_empty()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&ValueId> {
        Some(&self.graph.inputs[*self.use_list.get(index)?].value_id)
    }

    /// Iterator over each input slot's `ValueId` value.
    ///
    /// Same yield as `into_iter()` but borrows `self` so callers that
    /// only need a one-shot read don't have to move the `Inputs` value.
    pub fn iter(&self) -> impl Iterator<Item = ValueId> + Clone + 'a {
        let graph = self.graph;
        self.use_list.iter().map(move |id| graph.inputs[*id].value_id)
    }
}

impl<'a> IntoIterator for Inputs<'a> {
    type Item = ValueId;
    type IntoIter = InputIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        InputIter {
            graph: self.graph,
            iter: self.use_list.iter(),
        }
    }
}

impl Index<usize> for Inputs<'_> {
    type Output = ValueId;

    /// Index into the input slot list by position.
    ///
    /// # Panics
    ///
    /// Panics on out-of-bounds.  Prefer the fallible `get(idx)` or
    /// `node_inputs_exact::<N>(node)?` for opaque-arity nodes; for
    /// validator-pinned slots (e.g. `Call`'s slot 0 = ctrl, 1 = mem)
    /// the index is a documented invariant of the per-kind expected
    /// signature in `crate::node_signature`.
    fn index(&self, index: usize) -> &Self::Output {
        &self.graph.inputs[self.use_list[index]].value_id
    }
}

/// Concrete `IntoIter` for [`Inputs`].  Only nameable through
/// `<Inputs<'a> as IntoIterator>::IntoIter`; callers never construct
/// or name it directly (they get it implicitly from `for x in inputs`
/// or `.into_iter()`).  Prefer [`Inputs::iter`] when the borrow
/// shouldn't be consumed.
#[derive(Clone)]
pub struct InputIter<'a> {
    pub(crate) graph: &'a Graph,
    pub(crate) iter: slice::Iter<'a, UseId>,
}

impl Iterator for InputIter<'_> {
    type Item = ValueId;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.graph.inputs[*self.iter.next()?].value_id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

pub struct InputCursor<'a> {
    pub(crate) graph: &'a mut Graph,
    pub(crate) current: Option<UseId>,
}

impl InputCursor<'_> {
    pub fn current(&self) -> Option<(NodeId, u32)> {
        let current = &self.graph.inputs[self.current?];
        Some((current.node_id, current.input_index))
    }

    fn move_next(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        self.current = self.graph.inputs[current].next.expand();
    }

    pub fn replace_current_with(&mut self, new_value: ValueId) -> crate::error::Result<()> {
        let current = self
            .current
            .ok_or_else(|| anyhow!("attempted to replace a null cursor use"))?;
        self.move_next();
        self.graph.update_input(current, new_value);
        Ok(())
    }
}
