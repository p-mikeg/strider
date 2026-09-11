pub(crate) mod dot;
mod edit;
mod func;
mod side_tables;
pub use side_tables::{MemDecomp, MemoryId, SideTables};

pub use edit::EditFunction;
pub use edit::FunctionState;
pub use func::Function;
#[cfg(any(test, feature = "test-util"))]
pub use func::cc_ret_and_clobber_vns;

/// Trivial-convention [`Function`] with no tracked varnodes.
#[cfg(test)]
pub(crate) fn test_function() -> Function {
    let mut f = Function::new(
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Little,
        Vec::new(),
    );
    // Mirrors the `InitialMemory` the builder mints.
    f.graph_mut().create_node(
        crate::node::NodeKind::InitialMemory,
        [],
        [crate::node::ValueKind::Memory],
    );
    f
}

#[cfg(test)]
pub(crate) fn test_initial_memory(f: &Function) -> crate::node::NodeId {
    use crate::IRViewer;
    f.graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), crate::node::NodeKind::InitialMemory))
        .expect("test_function() builds an InitialMemory node")
}
