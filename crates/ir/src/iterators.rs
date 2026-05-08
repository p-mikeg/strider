use core::{ops::Index, slice};

use anyhow::anyhow;

use super::graph::Graph;
use super::node::{NodeId, NodeInputId, NodeOutputId};

#[derive(Clone, Copy)]
pub struct Outputs<'a>(pub(crate) &'a [NodeOutputId]);

impl Outputs<'_> {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<&NodeOutputId> {
        self.0.get(index)
    }
}

impl<'a> IntoIterator for Outputs<'a> {
    type Item = NodeOutputId;
    type IntoIter = OutputIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        OutputIter(self.0.iter())
    }
}

impl Index<usize> for Outputs<'_> {
    type Output = NodeOutputId;

    /// Index into the output slot list by position.
    ///
    /// # Panics
    ///
    /// Panics on out-of-bounds.  For fallible access prefer `iter()`,
    /// `node_outputs_exact::<N>(node)`, or
    /// `Graph::node_outputs(node).into_iter().nth(idx)`.  The validator
    /// (Layer A in `crate::validate`) pins the per-node-kind output
    /// arity, so `outputs[N]` for a known kind+slot is a documented
    /// invariant; arbitrary indices on opaque-arity nodes are not.
    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index]
    }
}

#[derive(Clone)]
pub struct OutputIter<'a>(slice::Iter<'a, NodeOutputId>);

impl Iterator for OutputIter<'_> {
    type Item = NodeOutputId;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().copied()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

// -------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub(crate) graph: &'a Graph,
    pub(crate) use_list: &'a [NodeInputId],
}

impl Inputs<'_> {
    pub fn len(&self) -> usize {
        self.use_list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<&NodeOutputId> {
        Some(&self.graph.inputs[*self.use_list.get(index)?].output_id)
    }
}

impl<'a> IntoIterator for Inputs<'a> {
    type Item = NodeOutputId;
    type IntoIter = InputIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        InputIter {
            graph: self.graph,
            iter: self.use_list.iter(),
        }
    }
}

impl Index<usize> for Inputs<'_> {
    type Output = NodeOutputId;

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
        &self.graph.inputs[self.use_list[index]].output_id
    }
}

#[derive(Clone)]
pub struct InputIter<'a> {
    pub(crate) graph: &'a Graph,
    pub(crate) iter: slice::Iter<'a, NodeInputId>,
}

impl Iterator for InputIter<'_> {
    type Item = NodeOutputId;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.graph.inputs[*self.iter.next()?].output_id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

#[derive(Clone)]
pub struct OutputUsageIter<'a> {
    pub(crate) graph: &'a Graph,
    pub(crate) cur_use: Option<NodeInputId>,
}

impl Iterator for OutputUsageIter<'_> {
    type Item = (NodeId, u32);

    fn next(&mut self) -> Option<Self::Item> {
        let use_data = &self.graph.inputs[self.cur_use?];
        self.cur_use = use_data.next.expand();
        Some((use_data.node_id, use_data.input_index))
    }
}

pub struct InputCursor<'a> {
    pub(crate) graph: &'a mut Graph,
    pub(crate) current: Option<NodeInputId>,
}

impl InputCursor<'_> {
    pub fn graph(&self) -> &Graph {
        self.graph
    }

    pub fn current(&self) -> Option<(NodeId, u32)> {
        let current = &self.graph.inputs[self.current?];
        Some((current.node_id, current.input_index))
    }

    pub fn move_next(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        self.current = self.graph.inputs[current].next.expand();
    }

    pub fn replace_current_with(&mut self, new_value: NodeOutputId) -> crate::error::Result<()> {
        let current = self
            .current
            .ok_or_else(|| anyhow!("attempted to replace a null cursor use"))?;
        self.move_next();
        self.graph.update_input(current, new_value);
        Ok(())
    }
}
