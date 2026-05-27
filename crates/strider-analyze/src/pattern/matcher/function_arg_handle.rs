//! Handle for a function argument carrier node accessed through [`Matcher`].

use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

/// Source classification derived from the underlying carrier node.
///
/// Returned by [`FunctionArgHandle::source`] when the caller needs to
/// distinguish register-passed from stack-passed arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgSource {
    /// The argument was passed in the given register varnode.
    Register(rsleigh::Vn),
    /// The argument was passed on the stack.  The offset is SP-relative
    /// at function entry.  Derived from the `Load`'s address expression
    /// when it can be decomposed to `InitialVar(sp) + K`.
    Stack,
    /// The carrier is neither an `InitialVar` nor a recognisable `Load`
    /// shape.  Rare in practice; included for forward-compatibility.
    Other,
}

/// A cheap reference to the carrier [`NodeId`] for a function argument,
/// accessed through [`crate::pattern::Matcher::function_arg`] or
/// [`crate::pattern::Matcher::function_args`].
///
/// The underlying node is an `InitialVar` (register arg) or `Load`
/// (stack arg) as recorded in [`Function::arg_index_to_nodes`] by
/// `FunctionArgDetect`.
#[derive(Clone, Copy)]
pub struct FunctionArgHandle<'g> {
    pub(super) node_id: NodeId,
    pub(super) function: &'g Function,
    pub(super) index: u32,
}

impl<'g> FunctionArgHandle<'g> {
    /// The underlying carrier node.
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// The primary value output of the carrier node.
    ///
    /// Register-arg carriers (`InitialVar`) have a single value output.
    /// Stack-arg carriers (`Load`) also have a single value output.
    #[must_use]
    pub fn output_id(&self) -> Option<NodeOutputId> {
        self.function.node_outputs(self.node_id).first().copied()
    }

    /// The argument's position in the calling convention.
    #[must_use]
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Classify the argument source by inspecting the carrier node's kind.
    ///
    /// * `InitialVar(vn)` → `Register(vn)`
    /// * `Load(_)` → `Stack`
    /// * anything else → `Other`
    #[must_use]
    pub fn source(&self) -> ArgSource {
        match *self.function.node_kind(self.node_id) {
            NodeKind::InitialVar(vn) => ArgSource::Register(vn),
            NodeKind::Load(_) => ArgSource::Stack,
            _ => ArgSource::Other,
        }
    }
}
