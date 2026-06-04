//! `Node<N>`, `ValueData<V>`, `UseData`, and `RawStore<N, V>` — the per-arena
//! entries that hold per-node payload, the use-list backbone, the stable
//! `(producer, output_index)` mapping, and the single place that mutates the
//! arenas.
//!
//! Generalized from `strider-ir`'s `node/data.rs` + `graph/{mod,store,uses}.rs`:
//! the baked-in `NodeKind` becomes the payload type parameter `N`, and
//! `ValueKind` becomes `V`. No `Hash`/`Eq` bound is imposed on `N`/`V` here —
//! `RawStore` only ever allocates and links, never compares payloads. A
//! caching policy that needs structural comparison lives in
//! [`crate::cache`] and adds its own bounds there.

use cranelift_entity::packed_option::PackedOption;
use cranelift_entity::{ListPool, PrimaryMap};
use smallvec::SmallVec;

use crate::ids::{NodeId, UseId, UseIdList, ValueId, ValueIdList};

/// Stores one output of a node and tracks all of its uses via an intrusive
/// doubly-linked list of [`UseData`] ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueData<V> {
    /// The payload describing what this output carries.
    pub(crate) kind: V,
    /// The node that produces this output.
    pub(crate) source_id: NodeId,
    /// The index of this output in the source node's output list.
    pub(crate) output_index: u32,
    /// Head of the linked list of all inputs that consume this output.
    pub(crate) first_use: PackedOption<UseId>,
}

impl<V> ValueData<V> {
    /// Creates a new `ValueData` with no uses yet.
    pub(crate) fn new(kind: V, source_id: NodeId, output_index: u32) -> Self {
        ValueData {
            kind,
            source_id,
            output_index,
            first_use: None.into(),
        }
    }
}

/// Records a single use of a [`ValueData`] as the input of some node.
///
/// Forms part of a doubly-linked list of all uses of a particular value,
/// enabling efficient update of all consumers when a value changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UseData {
    /// The value being consumed.
    pub(crate) value_id: ValueId,
    /// Previous use in the linked list of uses for `value_id`.
    pub(crate) prev: PackedOption<UseId>,
    /// Next use in the linked list of uses for `value_id`.
    pub(crate) next: PackedOption<UseId>,
    /// The node that consumes this input.
    pub(crate) node_id: NodeId,
    /// The position of this input in the consuming node's input list.
    pub(crate) input_index: u32,
}

impl UseData {
    /// Creates a new `UseData` not yet linked into any use list.
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

/// A node in the graph.
///
/// Holds the node's payload along with its input and output slot lists
/// (stored externally in entity pools).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node<N> {
    pub(crate) kind: N,
    pub(crate) inputs: UseIdList,
    pub(crate) outputs: ValueIdList,
}

impl<N> Node<N> {
    /// Creates a new node with the given payload and empty input/output lists.
    pub(crate) fn new(kind: N) -> Self {
        Self {
            kind,
            inputs: UseIdList::new(),
            outputs: ValueIdList::new(),
        }
    }
}

/// The structural arena backing a [`crate::graph::Graph`].
///
/// Owns the three `PrimaryMap`s (nodes, outputs, inputs) and the two pools
/// backing the per-node slot lists. This is the SINGLE place that mutates the
/// arenas; the graph's structural verbs and the caching policy both go through
/// these primitives.
pub struct RawStore<N, V> {
    /// Dense map from [`NodeId`] to [`Node`] metadata.
    pub(crate) nodes: PrimaryMap<NodeId, Node<N>>,
    /// Dense map from [`ValueId`] to [`ValueData`] metadata.
    pub(crate) outputs: PrimaryMap<ValueId, ValueData<V>>,
    /// Dense map from [`UseId`] to [`UseData`] metadata.
    pub(crate) inputs: PrimaryMap<UseId, UseData>,
    /// Pool backing the per-node output id lists.
    pub(crate) output_pool: ListPool<ValueId>,
    /// Pool backing the per-node input id lists.
    pub(crate) input_pool: ListPool<UseId>,
}

impl<N, V> RawStore<N, V> {
    /// Creates an empty store.
    pub(crate) fn new() -> Self {
        RawStore {
            nodes: PrimaryMap::new(),
            outputs: PrimaryMap::new(),
            inputs: PrimaryMap::new(),
            output_pool: ListPool::new(),
            input_pool: ListPool::new(),
        }
    }

    /// Allocates a fresh node with the given payload, input values, and output
    /// payloads.
    ///
    /// Records each input as a `UseData` entry linked into the use-list of the
    /// value it references, and allocates one `ValueData` per output payload.
    /// Always allocates a fresh [`NodeId`] — deduplication, if any, is the
    /// caching policy's job ([`crate::cache::NodeCacheable`]) and happens
    /// before this is called.
    ///
    /// Public because it is the primitive every [`crate::cache::NodeCacheable`]
    /// impl calls once it decides to allocate (rather than reuse) a node.
    pub fn alloc_node(
        &mut self,
        kind: N,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[V; 4]>,
    ) -> NodeId {
        let node_id = self.nodes.push(Node::new(kind));

        // Allocate the input `UseData` entries.
        let input_uses: SmallVec<[UseId; 4]> = inputs
            .into_iter()
            .enumerate()
            .map(|(index, value)| self.inputs.push(UseData::new(value, node_id, index as u32)))
            .collect();

        // Link each input into the use-list of the value it consumes.
        for &use_id in &input_uses {
            self.link_use_to_value_list(use_id);
        }

        // Allocate one output value per output payload.
        let output_values = outputs
            .into_iter()
            .enumerate()
            .map(|(index, kind)| self.outputs.push(ValueData::new(kind, node_id, index as u32)));

        self.nodes[node_id].inputs = UseIdList::from_iter(input_uses, &mut self.input_pool);
        self.nodes[node_id].outputs = ValueIdList::from_iter(output_values, &mut self.output_pool);

        node_id
    }

    // ── cacher-facing read accessors ────────────────────────────────────────
    //
    // These are the public surface a [`crate::cache::NodeCacheable`] impl needs
    // to recompute a node's structural key inside its `invalidate` / `rebuild`
    // hooks (its `kind`, the values driving its inputs, and its output kinds).

    /// Iterates over every node id in the arena, including unreachable ones.
    #[inline]
    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys()
    }

    /// Returns a reference to the payload of `node_id`.
    #[inline]
    pub fn kind_of(&self, node_id: NodeId) -> &N {
        &self.nodes[node_id].kind
    }

    /// Returns the values driving `node_id`'s inputs, in slot order.
    pub fn input_values(&self, node_id: NodeId) -> SmallVec<[ValueId; 4]> {
        self.node_input_uses(node_id)
            .iter()
            .map(|&use_id| self.inputs[use_id].value_id)
            .collect()
    }

    /// Returns the output payloads of `node_id`, in slot order.
    pub fn output_kinds(&self, node_id: NodeId) -> SmallVec<[V; 4]>
    where
        V: Clone,
    {
        self.node_outputs(node_id)
            .iter()
            .map(|&value_id| self.outputs[value_id].kind.clone())
            .collect()
    }

    // ── raw read accessors ──────────────────────────────────────────────────

    /// Returns a reference to the payload of `node_id`.
    #[inline]
    pub(crate) fn node_kind(&self, node_id: NodeId) -> &N {
        &self.nodes[node_id].kind
    }

    /// Returns the slice of input slot ids for `node_id`.
    #[inline]
    pub(crate) fn node_input_uses(&self, node_id: NodeId) -> &[UseId] {
        self.nodes[node_id].inputs.as_slice(&self.input_pool)
    }

    /// Returns the slice of output ids for `node_id`.
    #[inline]
    pub(crate) fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.nodes[node_id].outputs.as_slice(&self.output_pool)
    }

    /// Returns a reference to the payload of `value_id`.
    #[inline]
    pub(crate) fn value_kind(&self, value_id: ValueId) -> &V {
        &self.outputs[value_id].kind
    }

    /// Returns the node that produces `value_id`.
    #[inline]
    pub(crate) fn producer(&self, value_id: ValueId) -> NodeId {
        self.outputs[value_id].source_id
    }

    // ── use-list link / unlink ──────────────────────────────────────────────

    /// Inserts `input_id` at the head of the use-list of the value it
    /// references.
    ///
    /// Maintains the doubly-linked list stored inside [`UseData`] and
    /// [`ValueData`] so that all consumers of a value can be iterated.
    /// Callers guarantee `input_id` is freshly created (its `next`/`prev` are
    /// `None` by construction).
    pub(crate) fn link_use_to_value_list(&mut self, input_id: UseId) {
        let value_id = self.inputs[input_id].value_id;
        let next_value_use = self.outputs[value_id].first_use;

        self.inputs[input_id].next = next_value_use;
        if let Some(next_use) = next_value_use.expand() {
            self.inputs[next_use].prev = Some(input_id).into();
        }

        self.outputs[value_id].first_use = Some(input_id).into();
    }

    /// Removes `input_id` from the use-list of the value it references.
    ///
    /// After this call the `prev`/`next` pointers of `input_id` are cleared so
    /// the entry can be safely abandoned.
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
