//! Handle for a `FunctionArg` IR node accessed through [`Matcher`].

use ir::Graph;
use ir::node::{FunctionArgSource, NodeId, NodeOutputId, NodeOutputKind, NodeOutputType};

/// A cheap reference to a `FunctionArg` node within a specific
/// [`Graph`].
///
/// Returned by [`Matcher::function_arg`][super::Matcher::function_arg] and
/// [`Matcher::function_args`][super::Matcher::function_args].  The handle
/// caches the node's `source` and `index` at construction so the accessor
/// methods are infallible without a runtime `NodeKind` check.
#[derive(Clone, Copy)]
pub struct FunctionArgHandle<'g> {
    pub(super) graph: &'g Graph,
    pub(super) node_id: NodeId,
    pub(super) source: FunctionArgSource,
    pub(super) index: u32,
}

impl<'g> FunctionArgHandle<'g> {
    /// The underlying `NodeId` of the `FunctionArg` node.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// The `NodeOutputId` of this `FunctionArg`'s single value output.
    ///
    /// Returns `None` if the node has no outputs — this cannot happen for a
    /// correctly-constructed `FunctionArg`, but the method signature surfaces
    /// the possibility rather than panicking.
    pub fn output(&self) -> Option<NodeOutputId> {
        self.graph.node_outputs(self.node_id).into_iter().next()
    }

    /// The argument's ABI source (register or stack slot).
    pub fn source(&self) -> FunctionArgSource {
        self.source
    }

    /// The argument's position in the calling convention.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The declared output type (width) of this `FunctionArg`'s value.
    ///
    /// Returns `None` if the output kind is not a value type — this cannot
    /// happen for a correctly-constructed `FunctionArg`, but the method
    /// signature surfaces the possibility rather than panicking.
    pub fn width(&self) -> Option<NodeOutputType> {
        match self.graph.output_kind(self.output()?) {
            NodeOutputKind::OutputType(t) => Some(t),
            _ => None,
        }
    }
}
