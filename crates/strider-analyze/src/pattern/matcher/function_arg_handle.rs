//! Handle for a `FunctionArg` IR node accessed through [`Matcher`].

use std::marker::PhantomData;

use strider_ir::Graph;
use strider_ir::node::FunctionArgSource;

/// A cheap reference to a `FunctionArg` node within a specific
/// [`Graph`].
///
/// Returned by [`Matcher::function_arg`][super::Matcher::function_arg] and
/// [`Matcher::function_args`][super::Matcher::function_args].  The handle
/// caches the node's `source` and `index` at construction so the accessor
/// methods are infallible without a runtime `NodeKind` check.
#[derive(Clone, Copy)]
pub struct FunctionArgHandle<'g> {
    pub(super) source: FunctionArgSource,
    pub(super) index: u32,
    pub(super) _graph: PhantomData<&'g Graph>,
}

impl FunctionArgHandle<'_> {
    /// The argument's ABI source (register or stack slot).
    pub fn source(&self) -> FunctionArgSource {
        self.source
    }

    /// The argument's position in the calling convention.
    pub fn index(&self) -> u32 {
        self.index
    }
}
