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
//!   cache + `IntConst` payload normalisation, ported from the former
//!   `Graph::create_node`.
//! - [`IrGraphExt`] — the IR's typed / fallible accessors and the
//!   control-aware `walk_from` / `reverse_postorder` / `retain_reachable`
//!   that branch on `ValueKind::is_control` (and so cannot live in the
//!   payload-agnostic generic crate).
//! - The `Inputs` / `InputCursor` IR-payload aliases and the `VarTable`
//!   build-time interner.

use cranelift_entity::SecondaryMap;

use crate::node::{NodeId, NodeKind, ValueKind};

mod cache;
mod ext;

pub use cache::IrCacheable;
pub use ext::IrGraphExt;

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
/// `replace_all_uses`, the read accessors, …) are inherited from the generic
/// graph. The strider-specific typed/fallible accessors and the control-aware
/// walks come from [`IrGraphExt`] (bring it into scope with
/// `use crate::graph::IrGraphExt;`).
pub type Graph = strider_graph::Graph<NodeKind, ValueKind, IrCacheable>;

/// An iterable view over the input values of a node — the IR-payload
/// instantiation of [`strider_graph::Inputs`].
pub type Inputs<'a> = strider_graph::Inputs<'a, NodeKind, ValueKind, IrCacheable>;

/// A cursor over the use-list of a single value — the IR-payload
/// instantiation of [`strider_graph::InputCursor`].
pub type InputCursor<'a> = strider_graph::InputCursor<'a, NodeKind, ValueKind, IrCacheable>;

/// Remap-in-place trait for `SecondaryMap<NodeId, _>`-shaped side-tables.
///
/// Implementors expose a single method that rebuilds the table under the
/// old→new translation, draining the source via `std::mem::take` so the
/// post-remap source is left at `Default::default()` for every slot.
/// Used by [`crate::Function::compact`] to fold every `NodeId`-keyed
/// overlay table through one iteration site.
///
/// The Vn-keyed `initial_var_index` does **not** fit this shape (its
/// key is `rsleigh::Vn`, not `NodeId`) and is remapped inline in
/// `Function::compact`.
pub(crate) trait SideTableRemap {
    fn remap_node_keyed(&mut self, remap: &NodeIdRemap);
}

impl<T: Default + Clone> SideTableRemap for SecondaryMap<NodeId, T> {
    fn remap_node_keyed(&mut self, remap: &NodeIdRemap) {
        let mut dst: SecondaryMap<NodeId, T> = SecondaryMap::new();
        for (old_id, new_id) in remap.surviving_node_pairs() {
            dst[new_id] = std::mem::take(&mut self[old_id]);
        }
        *self = dst;
    }
}
