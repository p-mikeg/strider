//! Function-level graph helpers and their tests.
//!
//! [`crate::graph::Graph`] is the IR function graph.  After
//! [`crate::FunctionBuilder::build`] returns, the graph has its `entry`
//! and `cc_metadata` populated; pre-build construction uses
//! [`crate::graph::Graph::new`].

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
