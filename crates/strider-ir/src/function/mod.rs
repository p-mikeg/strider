//! The function data structures: the [`Function`] graph-plus-overlay
//! ([`data`]), the self-cleaning editing context [`EditFunction`] ([`edit`])
//! and its [`FunctionState`] bookkeeping ([`state`]), and the IR-specific dot
//! rendering ([`dot`]).

mod data;
pub(crate) mod dot;
mod edit;
mod state;

pub use data::Function;
pub(crate) use data::largest_container_in;
pub use edit::EditFunction;
pub use state::FunctionState;

/// The trivial-convention [`Function`] used throughout the in-crate tests.
///
/// [`Function::new`] builds the `Entry` node; this helper then mints the
/// `InitialMemory` node (normally `FunctionBuilder::build_entry`'s job) so the
/// result is a fully-formed (entry + memory) starting point with no tracked
/// varnodes.
#[cfg(test)]
pub(crate) fn test_function() -> Function {
    let mut f = Function::new(
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Little,
        Vec::new(),
        rustc_hash::FxHashMap::default(),
    );
    // The builder owns the memory spine; this test helper bypasses the builder,
    // so mint the `InitialMemory` node directly to mirror a built function.
    f.graph_mut().create_node(
        crate::node::NodeKind::InitialMemory,
        [],
        [crate::node::ValueKind::Memory],
    );
    f
}

/// The `InitialMemory` node of a [`test_function`]-shaped graph (the
/// auto-built node 1).  Convenience for in-crate tests that wire a `Memory`
/// edge into a Return / Call without re-deriving the skeleton node.
#[cfg(test)]
pub(crate) fn test_initial_memory(f: &Function) -> crate::node::NodeId {
    use crate::IRViewer;
    f.graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), crate::node::NodeKind::InitialMemory))
        .expect("test_function() builds an InitialMemory node")
}
