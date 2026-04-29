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
//! # Capture rule
//!
//! Every builder and every [`crate::pat::Pat`] supports `.capture(c)` via
//! the [`crate::pat::IntoPat`] blanket trait.  After a successful match,
//! [`crate::Match::node`] returns the bound `NodeId` and
//! [`crate::Match::output`] returns the bound value `NodeOutputId` (or
//! `None` for control-flow patterns that have no single value output).

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
