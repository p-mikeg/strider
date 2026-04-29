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

impl DoubleEndedIterator for OutputIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().copied()
    }
}

impl ExactSizeIterator for OutputIter<'_> {}

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

    fn index(&self, index: usize) -> &Self::Output {
        // Prefer `.get(index)` with proper error handling in production code.
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

impl DoubleEndedIterator for InputIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        Some(self.graph.inputs[*self.iter.next_back()?].output_id)
    }
}

impl ExactSizeIterator for InputIter<'_> {}

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
