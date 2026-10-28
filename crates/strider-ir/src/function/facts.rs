/// What a [`Function`] rests on: settled once and true for its whole life.
///
/// What makes a field one of these is that no operation touches it. Nothing
/// here is keyed by an arena id, so [`Function::compact`] leaves it alone, and
/// nothing here is derived from the graph, so
/// [`SideTables::invalidate_analyses`] never clears it. A derived, clearable
/// verdict is a memo and belongs in the memo group instead.
///
/// [`Function`]: crate::Function
/// [`Function::compact`]: crate::Function::compact
/// [`SideTables::invalidate_analyses`]: crate::SideTables::invalidate_analyses
#[derive(Clone)]
pub(crate) struct FunctionFacts {
    /// The convention this function was built under.
    pub(crate) default_cc: strider_target::BuiltCallingConvention,
    /// The byte order of each space's loads and stores.
    pub(crate) byte_order: strider_target::ByteOrder,
    /// The claims in force: every set [`Function::assume`] added, none before
    /// the first.  A claim is withdrawn by nothing.
    ///
    /// [`Function::assume`]: crate::Function::assume
    pub(crate) assumptions: crate::AssumptionOptions,
    /// The highest offset from the entry SP of a stack address held, at some
    /// `Call`, in a register the call clobbers, or `None` when no call held
    /// one.  Recorded once, by `FunctionBuilder::build`.  The call node carries
    /// no such value, so this is the only record that a callee may have
    /// received it.
    pub(crate) call_register_frame_reach: Option<i128>,
}
