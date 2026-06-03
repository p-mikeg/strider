//! [`CallDescriptor`] — per-Call / per-CallOther override descriptor stored
//! in [`crate::Function`]'s sparse `call_descriptor` side-table.
//!
//! For `Call` nodes whose target function uses a non-default calling
//! convention, the `Call` arm records the override
//! [`strider_target::BuiltCallingConvention`].  For `CallOther` nodes whose
//! ABI has been resolved from the target table, the `CallOther` arm records
//! the vn-resolved [`strider_target::BuiltCallOtherAbi`].
//!
//! Sparse by design: the default `Call` (function-default CC) and unmodeled
//! `CallOther` nodes record nothing, keeping the table small.

/// Per-call descriptor stored in `Function::call_descriptor`.
///
/// - `Call(cc)` — override calling convention for a `Call` node whose target
///   does not use the function-default ABI.  Consumers that only care about
///   Call-CC overrides can use the convenience accessor
///   [`crate::Function::call_cc`] which returns `Some` only for this arm.
/// - `CallOther(abi)` — vn-resolved footprint for a modeled `CallOther` node,
///   built from [`strider_target::call_other_abi::CallOtherAbi`] by the lifter once it has
///   access to the Sleigh register table.
#[derive(Clone, Debug)]
pub enum CallDescriptor {
    /// Override calling convention for a `NodeKind::Call` node.
    Call(strider_target::BuiltCallingConvention),
    /// Vn-resolved ABI footprint for a modeled `NodeKind::CallOther` node.
    CallOther(strider_target::BuiltCallOtherAbi),
}
