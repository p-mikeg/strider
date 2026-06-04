//! [`IrCacheable`] — the IR's dedup-or-create policy for the generic
//! [`strider_graph::Graph`].
//!
//! Ports the former `Graph::create_node` dedup cache (the
//! `(NodeKind, inputs, output_kinds)` → `NodeId` map plus the `is_cacheable`
//! gate) onto the generic [`strider_graph::NodeCacheable`] hooks:
//!
//! - `create` — normalises an `IntConst` payload to its declared width, then
//!   either returns an existing structurally-equal node or allocates a fresh
//!   one. This is the single source of truth for `IntConst` payload masking
//!   (a big-endian read can mint `IntConst(0xff..fc):I64` while another path
//!   mints the 64-bit-masked form; both are `-4:I64` and must dedup).
//! - `invalidate` — drops the dedup entry for a node whose input/output
//!   structure is about to change (so a `create` with the pre-change key can't
//!   resurrect the now-different node). Called by the generic mutation verbs
//!   before they mutate.
//! - `rebuild` — re-keys the whole cache over the surviving nodes after
//!   [`strider_graph::Graph::retain_reachable`] renumbers ids, and is re-run by
//!   [`crate::Function::compact`] after the wide-const GC rewrites
//!   `IntConstWide` payloads (which change those nodes' cache keys).

use hashbrown::HashMap;
use smallvec::SmallVec;
use strider_graph::{NodeCacheable, NodeId, RawStore, ValueId};

use crate::node::{NodeKind, ValueKind, ValueType};

/// The IR's deduplication cache: maps `(NodeKind, inputs, output_kinds)` →
/// `NodeId` for cacheable node kinds (see [`NodeKind::is_cacheable`]).
///
/// Non-cacheable kinds (`Region`, `Phi`, `MemPhi`, `Call`, …) are never
/// inserted — they always allocate a fresh node.
#[derive(Clone, Default)]
pub struct IrCacheable {
    node_to_id: HashMap<(NodeKind, Vec<ValueId>, Vec<ValueKind>), NodeId>,
}

impl IrCacheable {
    /// Normalises an `IntConst` payload by masking it to its declared integer
    /// output type's bit width, so every creation path (lifter sub-register
    /// read, rewrite closure, `build_int_const`, …) keys the cache on the same
    /// canonical narrow payload.
    ///
    /// Only the narrow integer `Typed` case is touched: wide constants
    /// (`I256`/`I512`) flow through `IntConstWide`, and non-integer /
    /// non-value outputs are left alone.
    fn normalize_kind(kind: NodeKind, outputs: &[ValueKind]) -> NodeKind {
        match (kind, outputs) {
            (NodeKind::IntConst(v), [ValueKind::Typed(ty)])
                if ty.is_integer() && !matches!(ty, ValueType::I256 | ValueType::I512) =>
            {
                NodeKind::IntConst(v & ty.bit_mask_u128())
            }
            (kind, _) => kind,
        }
    }
}

impl NodeCacheable<NodeKind, ValueKind> for IrCacheable {
    fn create(
        &mut self,
        store: &mut RawStore<NodeKind, ValueKind>,
        kind: NodeKind,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[ValueKind; 4]>,
    ) -> NodeId {
        let kind = Self::normalize_kind(kind, &outputs);

        if !kind.is_cacheable() {
            return store.alloc_node(kind, inputs, outputs);
        }

        let key = (kind, inputs.to_vec(), outputs.to_vec());
        if let Some(&existing) = self.node_to_id.get(&key) {
            return existing;
        }
        let node = store.alloc_node(kind, inputs, outputs);
        self.node_to_id.insert(key, node);
        node
    }

    fn invalidate(&mut self, node: NodeId, store: &RawStore<NodeKind, ValueKind>) {
        let kind = *store.kind_of(node);
        if !kind.is_cacheable() {
            return;
        }
        let key = (
            kind,
            store.input_values(node).into_iter().collect(),
            store.output_kinds(node).into_iter().collect(),
        );
        // Only drop the entry if it still maps to this node: a re-create of a
        // structurally-identical node may have re-pointed the key at a
        // different `NodeId`, and dropping that would defeat dedup.
        if self.node_to_id.get(&key) == Some(&node) {
            self.node_to_id.remove(&key);
        }
    }

    fn rebuild(&mut self, store: &RawStore<NodeKind, ValueKind>) {
        self.node_to_id.clear();
        for node in store.node_ids() {
            let kind = *store.kind_of(node);
            if !kind.is_cacheable() {
                continue;
            }
            let key = (
                kind,
                store.input_values(node).into_iter().collect(),
                store.output_kinds(node).into_iter().collect(),
            );
            // Last writer wins on the (impossible-by-construction) collision;
            // reachable nodes with identical keys are already deduped.
            self.node_to_id.entry(key).or_insert(node);
        }
    }
}
