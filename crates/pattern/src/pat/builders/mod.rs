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
//! - Value-producing builders (memory, phi, function-arg, binary ops,
//!   wildcards) expose `.capture(v: Var)` through [`crate::pat::IntoPat`].
//!   It binds the matched `NodeOutputId` — value-kind filtered, so on a
//!   multi-output node (e.g. `Load` = `[Memory, Value]`) the capture always
//!   lands on the value slot.
//! - Control-flow builders (`CallPat`, `IfPat`, `RetPat`, `CallOtherPat`)
//!   expose `.capture_node(nv: NodeVar)` to bind the matched `NodeId`.
//!   These sites have no single "the value" output, so `NodeVar` is the
//!   only handle.
//!
//! No builder exposes both.  A user never has to choose between them on a
//! given builder.

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
