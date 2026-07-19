use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{ListPool, PrimaryMap};
use smallvec::SmallVec;

use crate::ids::{NodeId, UseId, UseIdList, ValueId, ValueIdList};

/// One output of a node, tracking all of its uses via an intrusive
/// doubly-linked list of [`UseData`] ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ValueData<V> {
    pub(crate) kind: V,
    pub(crate) source_id: NodeId,
    pub(crate) output_index: u32,
    /// Head of the use-list: every input consuming this output.
    pub(crate) first_use: PackedOption<UseId>,
}

impl<V> ValueData<V> {
    pub(crate) fn new(kind: V, source_id: NodeId, output_index: u32) -> Self {
        ValueData {
            kind,
            source_id,
            output_index,
            first_use: None.into(),
        }
    }
}

/// One use of a [`ValueData`] as some node's input. Doubly linked with every
/// other use of the same value, so all consumers can be updated in one walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UseData {
    pub(crate) value_id: ValueId,
    prev: PackedOption<UseId>,
    pub(crate) next: PackedOption<UseId>,
    /// The consuming node.
    pub(crate) node_id: NodeId,
    pub(crate) input_index: u32,
}

impl UseData {
    /// Not yet linked into any use-list.
    pub(crate) fn new(value_id: ValueId, node_id: NodeId, input_index: u32) -> Self {
        UseData {
            value_id,
            prev: None.into(),
            next: None.into(),
            node_id,
            input_index,
        }
    }
}

/// Payload plus input/output slot lists, the lists themselves pooled
/// externally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Node<N> {
    pub(crate) kind: N,
    pub(crate) inputs: UseIdList,
    pub(crate) outputs: ValueIdList,
}

impl<N> Node<N> {
    pub(crate) fn new(kind: N) -> Self {
        Self {
            kind,
            inputs: UseIdList::new(),
            outputs: ValueIdList::new(),
        }
    }
}

/// The structural arena backing a [`crate::graph::Graph`].
#[derive(Clone)]
pub struct RawStore<N, V> {
    pub(crate) nodes: PrimaryMap<NodeId, Node<N>>,
    pub(crate) outputs: PrimaryMap<ValueId, ValueData<V>>,
    pub(crate) inputs: PrimaryMap<UseId, UseData>,
    pub(crate) output_pool: ListPool<ValueId>,
    pub(crate) input_pool: ListPool<UseId>,
}

impl<N, V> RawStore<N, V> {
    pub(crate) fn new() -> Self {
        RawStore {
            nodes: PrimaryMap::new(),
            outputs: PrimaryMap::new(),
            inputs: PrimaryMap::new(),
            output_pool: ListPool::new(),
            input_pool: ListPool::new(),
        }
    }

    /// ALWAYS allocates a fresh [`NodeId`]; never deduplicates.
    pub(crate) fn alloc_node(
        &mut self,
        kind: N,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[V; 4]>,
    ) -> NodeId {
        let node_id = self.nodes.push(Node::new(kind));

        let input_uses: SmallVec<[UseId; 4]> = inputs
            .into_iter()
            .enumerate()
            .map(|(index, value)| self.inputs.push(UseData::new(value, node_id, index as u32)))
            .collect();

        for &use_id in &input_uses {
            self.link_use_to_value_list(use_id);
        }

        let output_values = outputs.into_iter().enumerate().map(|(index, kind)| {
            self.outputs
                .push(ValueData::new(kind, node_id, index as u32))
        });

        self.nodes[node_id].inputs = UseIdList::from_iter(input_uses, &mut self.input_pool);
        self.nodes[node_id].outputs = ValueIdList::from_iter(output_values, &mut self.output_pool);

        node_id
    }

    /// Every node id in the arena, including unreachable ones.
    #[inline]
    pub(crate) fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys()
    }

    #[inline]
    pub fn kind_of(&self, node_id: NodeId) -> &N {
        &self.nodes[node_id].kind
    }

    /// The values driving `node_id`'s inputs, in slot order.
    pub fn input_values(&self, node_id: NodeId) -> SmallVec<[ValueId; 4]> {
        self.node_input_uses(node_id)
            .iter()
            .map(|&use_id| self.inputs[use_id].value_id)
            .collect()
    }

    /// The output payloads of `node_id`, in slot order.
    pub fn output_kinds(&self, node_id: NodeId) -> SmallVec<[V; 4]>
    where
        V: Clone,
    {
        self.node_outputs(node_id)
            .iter()
            .map(|&value_id| self.outputs[value_id].kind.clone())
            .collect()
    }

    #[inline]
    pub(crate) fn node_kind_mut(&mut self, node_id: NodeId) -> &mut N {
        &mut self.nodes[node_id].kind
    }

    #[inline]
    pub(crate) fn node_input_uses(&self, node_id: NodeId) -> &[UseId] {
        self.nodes[node_id].inputs.as_slice(&self.input_pool)
    }

    #[inline]
    pub(crate) fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.nodes[node_id].outputs.as_slice(&self.output_pool)
    }

    #[inline]
    pub(crate) fn value_kind(&self, value_id: ValueId) -> &V {
        &self.outputs[value_id].kind
    }

    #[inline]
    pub(crate) fn producer(&self, value_id: ValueId) -> NodeId {
        self.outputs[value_id].source_id
    }

    /// Inserts `input_id` at the head of its value's use-list.
    ///
    /// Callers guarantee `input_id` is freshly created, so its `prev`/`next`
    /// are `None` by construction.
    pub(crate) fn link_use_to_value_list(&mut self, input_id: UseId) {
        let value_id = self.inputs[input_id].value_id;
        let next_value_use = self.outputs[value_id].first_use;

        self.inputs[input_id].next = next_value_use;
        if let Some(next_use) = next_value_use.expand() {
            self.inputs[next_use].prev = Some(input_id).into();
        }

        self.outputs[value_id].first_use = Some(input_id).into();
    }

    /// Clears `input_id`'s `prev`/`next` afterwards, so the entry can be
    /// safely abandoned.
    pub(crate) fn unlink_use_from_value_list(&mut self, input_id: UseId) {
        let (value_id, prev, next) = {
            let input = &self.inputs[input_id];
            (input.value_id, input.prev, input.next)
        };
        let output = &mut self.outputs[value_id];

        if output.first_use.expand() == Some(input_id) {
            output.first_use = next;
        }
        if let Some(prev) = prev.expand() {
            self.inputs[prev].next = next;
        }
        if let Some(next) = next.expand() {
            self.inputs[next].prev = prev;
        }

        self.inputs[input_id].prev = None.into();
        self.inputs[input_id].next = None.into();
    }
}

impl<N, V> Default for RawStore<N, V> {
    fn default() -> Self {
        Self::new()
    }
}
