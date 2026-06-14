//! [`FunctionState`] — the self-cleaning edit context's persistent
//! bookkeeping: the live-node set, the input-less `roots` (the seeds of a
//! real reverse-post-order), the maybe-dead `queue`, and per-node
//! [`NodeFlags`].
//!
//! [`FunctionState::populate`] is a **pure read** over a built [`Function`]:
//! it seeds `live_nodes` + `roots` from
//! [`crate::walk::GraphWalkInfo::compute_full`] and leaves the queue
//! and flags empty.  The pre-existing-dead cull (which needs `&mut`) is the
//! explicit [`EditFunction::cull_dead`](super::EditFunction::cull_dead), not
//! here.

use crate::Function;
use crate::node::NodeId;
use cranelift_entity::SecondaryMap;
use entity_utils::{DenseEntitySet, Worklist};

bitflags::bitflags! {
    /// Per-node rewrite-state flags.
    ///
    /// * `ENQUEUED` — the node is currently sitting in the maybe-dead queue.
    /// * `OUTPUT_KILLED` — a use of one of the node's outputs was detached
    ///   while it was the last use, so the node *may* now be dead and must be
    ///   re-examined when drained.
    /// * `NEEDS_RECANON` — the node's inputs changed, so it may now be a
    ///   structural twin of an existing node and must be re-canonicalized when
    ///   drained. This is spidir's `CANONICAL` bit inverted: the graph starts
    ///   fully canonical (every node was minted through the construction dedup
    ///   cache), so the natural dual is a set-on-dirty flag needing no O(n)
    ///   initialization.
    #[derive(Clone, Copy, Default)]
    pub(crate) struct NodeFlags: u8 {
        const ENQUEUED = 0b01;
        const OUTPUT_KILLED = 0b10;
        const NEEDS_RECANON = 0b100;
    }
}

/// Persistent edit bookkeeping owned by an [`EditFunction`](super::EditFunction):
/// [`EditFunction::new`](super::EditFunction) builds one via
/// `FunctionState::populate` and keeps it for the context's lifetime.
///
/// The type stays `pub` (it appears in the owning field's signature), but the
/// fields are `pub(crate)` so the bookkeeping is an opaque handle outside this
/// crate.
pub struct FunctionState {
    /// Every node currently considered live (entry-reachable, not culled).
    pub(crate) live_nodes: DenseEntitySet<NodeId>,
    /// Input-less source nodes — the seeds of the cached reverse-post-order.
    /// Maintained in O(1) per edit (insert/remove/contains are O(1)); iterated
    /// in ascending-`NodeId` order.
    pub(crate) roots: DenseEntitySet<NodeId>,
    /// Nodes whose liveness may have just dropped; drained by `clean`.
    pub(crate) queue: Worklist<NodeId>,
    /// Per-node rewrite-state flags.
    pub(crate) flags: SecondaryMap<NodeId, NodeFlags>,
}

impl FunctionState {
    /// Seed `live_nodes` + `roots` from a built [`Function`]'s entry-reachable
    /// walk.  Pure read: the queue and flags start empty, and no node is
    /// culled (that needs `&mut` and is the explicit
    /// [`EditFunction::cull_dead`](super::EditFunction::cull_dead)).
    ///
    /// # Panics
    ///
    /// Panics if `function` has not been built (no entry node).
    #[allow(clippy::expect_used)]
    pub(crate) fn populate(function: &Function) -> Self {
        let entry = function
            .entry()
            .expect("FunctionState::populate: built function has an entry");
        let info = crate::walk::GraphWalkInfo::compute_full(function.graph(), entry);
        let mut roots: DenseEntitySet<NodeId> = DenseEntitySet::new();
        for r in info.roots {
            roots.insert(r);
        }
        Self {
            live_nodes: info.live_nodes,
            roots,
            queue: Worklist::new(),
            flags: SecondaryMap::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::FunctionState;
    use crate::IRViewer;
    use crate::IntBinaryOp;
    use crate::ValueType;
    use crate::builder::IRBuilderExt;
    use crate::function::edit::test_fixtures::single_region_builder;
    use crate::node::NodeKind;

    /// `populate` seeds `roots` with exactly the input-less reachable nodes
    /// (`Entry` + the two operand consts) and excludes a dangling
    /// unreachable const from the live set.
    #[test]
    fn populate_seeds_roots_and_live_set() {
        let mut b = single_region_builder();

        b.set_lift_addr(Some(0x10));
        let k1 = b.build_int_const(7u64, ValueType::I64).unwrap();
        let k2 = b.build_int_const(11u64, ValueType::I64).unwrap();
        let sum = b
            .build_int_binary_operation(k1, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        // Dangling, unreachable const: created but never wired into anything.
        let dangling = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
        b.set_lift_addr(None);
        let function = b.build().unwrap();

        let k1_node = function.producer(k1);
        let k2_node = function.producer(k2);
        let dangling_node = function.producer(dangling);
        let entry = function.entry().unwrap();

        let state = FunctionState::populate(&function);

        // Every root is input-less.
        for r in state.roots.iter() {
            assert!(
                function.graph().node_inputs(r).is_empty(),
                "root {r:?} must be input-less"
            );
        }

        // Entry and both operand consts are roots.
        assert!(state.roots.contains(entry), "Entry must be a root");
        assert!(state.roots.contains(k1_node), "k1 const must be a root");
        assert!(state.roots.contains(k2_node), "k2 const must be a root");

        // The dangling const is excluded from the live set.
        assert!(
            !state.live_nodes.contains(dangling_node),
            "dangling unreachable const must not be live"
        );
        // Sanity: it's a distinct const node (not deduped with k1/k2).
        assert!(
            matches!(function.node_kind(dangling_node), NodeKind::IntConst(_)),
            "dangling node is an IntConst"
        );

        // The queue and flags start empty.
        assert!(state.queue.is_empty(), "queue starts empty");
    }
}
