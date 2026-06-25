//! The IR sea-of-nodes [`Graph`] — a type alias over the generic
//! [`strider_graph::Graph`] parameterised with the IR payloads
//! ([`crate::node::NodeKind`] / [`crate::node::ValueKind`]) and the IR's
//! dedup policy ([`cache::IrCacheable`]).
//!
//! The structural machinery (node arena, use-lists, compaction, structural
//! walks, `Inputs` / `InputCursor` navigation) lives in `strider-graph`. This
//! module supplies only the strider-specific overlay:
//!
//! - [`cache::IrCacheable`] — the `(NodeKind, inputs, output_kinds)` dedup
//!   policy (`should_cache` / `hash` / `eq`).  It is purely mechanical: it
//!   embeds no domain normalisation.  Integer-constant canonicalisation
//!   (masking + small→wide promotion) happens at construction in
//!   `Function::create_node_attributed`, before a node reaches the cache.
//! - The `Inputs` / `InputCursor` IR-payload aliases and the `VarTable`
//!   build-time interner.
//!
//! The typed / fallible structural accessors (`node_outputs_exact` /
//! `node_inputs_exact` / `node_input_id_at`) are inherent on the generic
//! [`strider_graph::Graph`]; the function-overlay reads and the control-aware
//! walks live on [`crate::IRViewer`] / [`crate::IRWalker`].

use cranelift_entity::SecondaryMap;

use crate::node::{NodeId, NodeKind, ValueKind};

mod cache;

pub use cache::IrCacheable;

// The id translation table is structural — it comes from `strider-graph`.
pub use strider_graph::NodeIdRemap;

#[cfg(test)]
mod tests;

/// Bidirectional tracked-variable table (`VarId ↔ Vn`): the forward
/// `VarId → Vn` map plus its `Vn → VarId` reverse index, kept consistent by
/// construction.  An [`entity_utils::EntityInterner`] — `intern` is the sole
/// mutator (writes both halves), `key_of`/`get` resolve either direction in
/// O(1), and `keys()`/`values()` iterate in insertion (`VarId`) order for
/// the consumers that need ABI slot order.
///
/// This is a **build-time-only** type: it lives on the
/// [`crate::FunctionBuilder`] for SSA bookkeeping while the function is
/// being constructed.  It is **not** stored on the finished
/// [`crate::Function`] — the post-build varnode record is the ordered
/// `crate::Function::all_vns` list (snapshotted from this table in
/// `new`, one entry per tracked variable) instead.
pub(crate) type VarTable = entity_utils::EntityInterner<crate::builder::VarId, rsleigh::Vn>;

/// The IR sea-of-nodes graph.
///
/// A [`strider_graph::Graph`] over the IR node payload ([`NodeKind`]), the IR
/// value payload ([`ValueKind`]), and the IR dedup policy ([`IrCacheable`]).
/// Cacheable node kinds (see [`NodeKind::is_cacheable`]) are deduplicated by
/// `(NodeKind, inputs, output_kinds)`; non-cacheable kinds always allocate a
/// fresh [`NodeId`].
///
/// All structural verbs (`create_node`, `add_node_input`, `update_input`,
/// `replace_all_uses`, the read accessors, the typed `node_outputs_exact` /
/// `node_inputs_exact` / `node_input_id_at`, …) are inherited from the generic
/// graph. The function-overlay reads and control-aware walks live on
/// [`crate::IRViewer`] / [`crate::IRWalker`].
pub type Graph = strider_graph::Graph<NodeKind, ValueKind, IrCacheable>;

/// An iterable view over the input values of a node — the IR-payload
/// instantiation of [`strider_graph::Inputs`].
pub type Inputs<'a> = strider_graph::Inputs<'a, NodeKind, ValueKind, IrCacheable>;

/// A cursor over the use-list of a single value — the IR-payload
/// instantiation of [`strider_graph::InputCursor`].
pub type InputCursor<'a> = strider_graph::InputCursor<'a, NodeKind, ValueKind, IrCacheable>;

/// Rebuilds a `SecondaryMap<NodeId, _>`-shaped side-table in place under the
/// old→new translation, draining the source via `std::mem::take` so the
/// post-remap source is left at `Default::default()` for every slot.
/// Used by [`crate::Function::compact`] to fold every `NodeId`-keyed
/// overlay table through one iteration site.
///
/// The Vn-keyed `initial_var_index` does **not** fit this shape (its
/// key is `rsleigh::Vn`, not `NodeId`) and is remapped inline in
/// `Function::compact`.
pub(crate) fn remap_node_keyed<T: Default + Clone>(
    map: &mut SecondaryMap<NodeId, T>,
    remap: &NodeIdRemap,
) {
    let mut dst: SecondaryMap<NodeId, T> = SecondaryMap::new();
    for (old_id, new_id) in remap.surviving_node_pairs() {
        dst[new_id] = std::mem::take(&mut map[old_id]);
    }
    *map = dst;
}
