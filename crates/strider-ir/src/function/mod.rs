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
/// [`Function::new`] is the single SSoT constructor; it builds the
/// Entry + InitialMemory skeleton (nodes 0 and 1) automatically, so this
/// is a fully-formed (entry-bearing) starting point with no tracked
/// varnodes.
#[cfg(test)]
pub(crate) fn test_function() -> Function {
    Function::new(
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Little,
        Vec::new(),
        rustc_hash::FxHashMap::default(),
    )
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
