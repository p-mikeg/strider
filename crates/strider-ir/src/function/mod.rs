mod assumptions;
pub(crate) mod dot;
mod edit;
mod func;
mod memos;
mod side_tables;
mod text;
pub use memos::{MemDecomp, MemoryId};
pub use side_tables::SideTables;

pub use assumptions::AssumptionOptions;
pub use edit::EditFunction;
pub use func::Function;
#[cfg(any(test, feature = "test-util"))]
pub use func::cc_ret_and_clobber_vns;
pub(crate) use text::{space_str, vn_str};

/// Trivial-convention [`Function`] tracking `vns`.
#[cfg(test)]
pub(crate) fn test_function_tracking(vns: Vec<rsleigh::Vn>) -> Function {
    let mut f = Function::new(
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Little,
        vns,
    );
    // Mirrors the `InitialMemory` the builder mints.
    f.graph_mut().create_node(
        crate::node::NodeKind::InitialMemory,
        [],
        [crate::node::ValueKind::Memory],
    );
    f
}

/// Trivial-convention [`Function`] with no tracked varnodes.
#[cfg(test)]
pub(crate) fn test_function() -> Function {
    test_function_tracking(Vec::new())
}

#[cfg(test)]
pub(crate) fn test_initial_memory(f: &Function) -> crate::node::NodeId {
    use crate::IRViewer;
    f.graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), crate::node::NodeKind::InitialMemory))
        .expect("test_function() builds an InitialMemory node")
}
