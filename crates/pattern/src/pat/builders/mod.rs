//! Builder structs for [`crate::pat::Pat`], grouped by family.
//!
//! Every builder produces a [`crate::pat::node_pat::NodePat`] via
//! `NodePat::matcher(...)` plus the `.with_*` fluent setters.  Data builders
//! (`IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat`, the memory
//! family, `PhiPat`, `FunctionArgPat`) use `InputsSpec::Fixed` or
//! `InputsSpec::Indexed`; control builders (`CallPat`, `CallOtherPat`,
//! `RetPat`, `IfPat`) use `InputsSpec::Indexed` plus, for `If`, the
//! `ConsumersSpec::Indexed` direct-step forward walk for branch successors.
//!
//! Builders that expose `capture_output(v)` / `capture_node(nv)` implement
//! the [`CaptureBuilder`] trait to share a single pair of setter methods.

use crate::var::{NodeVar, Var};

mod binary_op;
mod branch;
mod call;
mod function_arg;
mod memory;
mod phi;
mod ret;

pub use binary_op::{BoolBinaryOpPat, FloatBinaryOpPat, IntBinaryOpPat};
pub use branch::IfPat;
pub use call::{CallOtherPat, CallPat};
pub use function_arg::FunctionArgPat;
pub use memory::{LoadPat, StackStorePat, StackStorePhiPat, StorePat};
pub use phi::PhiPat;
pub use ret::RetPat;

// ── Shared trait: capture_output / capture_node ───────────────────────────────

/// Shared plumbing for builder types that bind the matched
/// `NodeOutputId` and/or `NodeId` to a capture variable.
///
/// Implementing types expose `&mut Option<Var>` and `&mut Option<NodeVar>`
/// slots; the trait provides the fluent `capture_output` / `capture_node`
/// setters so each builder does not re-write them.
pub trait CaptureBuilder: Sized {
    fn output_slot(&mut self) -> &mut Option<Var>;
    fn node_slot(&mut self) -> &mut Option<NodeVar>;

    /// Bind the matched node's primary value output (`NodeOutputId`) to `v`.
    fn capture_output(mut self, v: Var) -> Self {
        *self.output_slot() = Some(v);
        self
    }

    /// Bind the matched node's id (`NodeId`) to `nv`.
    fn capture_node(mut self, nv: NodeVar) -> Self {
        *self.node_slot() = Some(nv);
        self
    }
}
