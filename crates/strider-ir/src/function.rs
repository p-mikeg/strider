use crate::graph::Graph;
use crate::node::{NodeId, NodeOutputId};
use cranelift_entity::packed_option::ReservedValue;

/// An under-construction IR function graph.
///
/// Holds the node graph together with the entry-node ids that anchor the
/// control-flow and memory chains.  Call [`crate::FunctionBuilder::build`]
/// to consume a `FunctionGraph` and produce a finalised [`Graph`] whose
/// `entry` and `cc_metadata` fields are populated.
#[derive(Clone)]
pub struct FunctionGraph {
    /// The sea-of-nodes graph being built.
    pub graph: Graph,
    /// The `Entry` node that serves as the root of the function.
    pub entry: NodeId,
    /// The single `Control` output of the `Entry` node.
    pub entry_control: NodeOutputId,
    /// The single `Memory` output of the `InitialMemory` node.
    pub entry_memory: NodeOutputId,
}

impl FunctionGraph {
    /// Creates a `FunctionGraph` with all ids set to their reserved
    /// (invalid) sentinel values.  Used as a placeholder by the builder
    /// before the real entry nodes are emitted; not part of the public
    /// surface because consumers should never observe a partial graph.
    pub(crate) fn new_invalid() -> Self {
        Self {
            graph: Graph::new(),
            entry: NodeId::reserved_value(),
            entry_control: NodeOutputId::reserved_value(),
            entry_memory: NodeOutputId::reserved_value(),
        }
    }
}

/// A fully-built IR function graph ready for analysis.
///
/// Type alias for [`Graph`] preserved for source compatibility — the
/// previous `BuiltFunctionGraph` newtype wrapper was a `Deref<Target =
/// Graph>` whose every accessor unwrapped `Option`s on the inner
/// `Graph`'s `entry` / `cc_metadata` fields.  Those fields are now part
/// of `Graph` itself; the wrapper's only contribution was a type-level
/// "is built" hint that opt-pass call sites (taking `&mut Graph`)
/// already lacked.  After [`crate::FunctionBuilder::build`] returns, the
/// `Graph` has populated `entry` and `cc_metadata`; pre-build construction
/// uses `Graph::new`.
pub type BuiltFunctionGraph = Graph;

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
        let entry_id = graph.entry();
        let outs: Vec<_> = graph.node_outputs(entry_id).iter().copied().collect();
        assert_eq!(outs.len(), 1);
        assert!(graph.output_kind(outs[0]).is_control());
    }
}
