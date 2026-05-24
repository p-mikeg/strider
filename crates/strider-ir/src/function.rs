//! [`Function`] — a [`Graph`] plus per-function overlay state (entry,
//! cc_metadata, side tables).
//!
//! [`Graph`] holds structural state (nodes/edges/wide_const interning, dedup
//! cache).  [`Function`] holds the overlay that gives those nodes their
//! function-level meaning: which node is the entry, the calling convention
//! metadata, asm fingerprint attribution, and other `NodeId`-keyed side
//! tables.
//!
//! Passes that only need structure take `&Graph`; passes that need the overlay
//! (most opt passes, the validator, dot rendering) take `&Function` or
//! `&mut Function`.
//!
//! Storage of the side tables is being progressively moved from [`Graph`] onto
//! [`Function`] across a series of follow-up commits.  Today,
//! [`Function::asm_fingerprint`] delegates to `Graph`'s storage; a subsequent
//! commit will move the storage onto `Function`.

use crate::graph::Graph;
use crate::node::NodeId;

/// A lifted function: structural [`Graph`] plus per-function overlay state.
///
/// Construct an empty `Function` with [`Function::new`]; populate the graph
/// via [`Function::graph_mut`]; mark the entry node with [`Function::set_entry`].
///
/// The completed, optimised form is produced by `FunctionBuilder::build` once
/// the overlay migration is finished.
#[derive(Default)]
pub struct Function {
    graph: Graph,
    entry: Option<NodeId>,
}

impl Function {
    /// Creates a `Function` with an empty graph and no entry node.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a shared reference to the underlying graph.
    #[inline]
    #[must_use]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Returns a mutable reference to the underlying graph.
    #[inline]
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    /// Returns the entry node, if one has been recorded.
    #[inline]
    #[must_use]
    pub fn entry(&self) -> Option<NodeId> {
        self.entry
    }

    /// Records `entry` as the function's entry node.
    pub fn set_entry(&mut self, entry: NodeId) {
        self.entry = Some(entry);
    }

    /// Returns the asm-fingerprint addresses attributed to `id`.
    ///
    /// Storage lives on [`Graph`]; a subsequent commit moves it onto
    /// [`Function`].
    #[must_use]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.graph.asm_fingerprint(id)
    }

    /// Sets the asm-fingerprint for `id` to `fp` (sorted, deduplicated).
    ///
    /// Storage lives on [`Graph`]; a subsequent commit moves it onto
    /// [`Function`].
    pub fn set_asm_fingerprint(&mut self, id: NodeId, fp: Vec<u64>) {
        self.graph.set_asm_fingerprint(id, fp);
    }
}

#[cfg(test)]
mod function_skeleton_tests {
    use super::Function;
    use crate::node::{NodeKind, NodeOutputKind};

    #[test]
    fn function_new_carries_an_empty_graph() {
        let f = Function::new();
        assert_eq!(f.graph().all_node_ids().count(), 0);
        assert!(f.entry().is_none());
    }

    #[test]
    fn function_records_entry_via_set_entry() {
        let mut f = Function::new();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        f.set_entry(entry);
        assert_eq!(f.entry(), Some(entry));
    }

    #[test]
    fn function_asm_fingerprint_round_trips() {
        let mut f = Function::new();
        let n = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        f.set_asm_fingerprint(n, vec![0xDEAD_BEEF]);
        assert_eq!(f.asm_fingerprint(n), &[0xDEAD_BEEF]);
    }
}

#[cfg(test)]
mod compact_tests {
    #![allow(clippy::unwrap_used)]

    use crate::graph::CcMetadata;
    use crate::node::{NodeKind, NodeOutputKind};
    use cranelift_entity::PrimaryMap;

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut graph = crate::graph::Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _zombie = graph.create_node(
            NodeKind::IntConst(0xdead),
            [],
            [NodeOutputKind::OutputType(crate::node::NodeOutputType::U64)],
        );
        graph.entry = Some(entry);
        graph.cc_metadata = Some(CcMetadata {
            variables: PrimaryMap::new(),
            call_clobbered: Box::new([]),
            ret_val_regs: Box::new([]),
            call_other_clobbered: Box::new([]),
            no_memory_clobber: false,
        });
        let pre_count = graph.all_node_ids().count();

        let _remap = graph.compact().expect("compact succeeds on a valid graph");

        let post_count = graph.all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let entry_id = graph.entry().unwrap();
        let outs: Vec<_> = graph.node_outputs(entry_id).to_vec();
        assert_eq!(outs.len(), 1);
        assert!(graph.output_kind(outs[0]).is_control());
    }
}
