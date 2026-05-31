//! Builder structs for [`crate::pattern::pat::Pat`], grouped by family.
//!
//! Most builders produce a [`crate::pattern::pat::node_pat::NodePat`] via
//! `NodePat::matcher(...)` plus the `.with_*` fluent setters.  Data builders
//! (`IntBinaryOpPat`, `FloatBinaryOpPat`, the memory
//! family, `PhiPat`, `FunctionArgPat`) use `InputsSpec::Fixed` or
//! `InputsSpec::Indexed`; control builders (`CallPat`, `CallOtherPat`,
//! `RetPat`) use `InputsSpec::Indexed`.  `IfPat` is the exception — it
//! uses a custom `Pattern` impl so it can navigate the `If` node's two
//! control outputs to their respective branch consumers.  The
//! compiler-inverted layout (`If(BitNot(C)){B}{A}`) is canonicalised
//! upstream by the `opt::IfCondInversion` pass, so `IfPat` matches the
//! direct layout only.
//!
//! # Capture rule
//!
//! Every builder and every [`crate::pattern::pat::Pat`] supports `.capture(c)` via
//! the [`crate::pattern::pat::IntoPat`] blanket trait.  After a successful match,
//! [`crate::pattern::Match::node`] returns the bound `NodeId` and
//! [`crate::pattern::Match::output`] returns the bound value `NodeOutputId` (or
//! `None` for control-flow patterns that have no single value output).

mod binary_op;
mod branch;
mod call;
mod cmp_op;
mod function_arg;
mod memory;
mod phi;
mod ret;
mod unary_op;

pub use binary_op::{BinaryOpPat, BoolBinaryOpPat, FloatBinaryOpPat, IntBinaryOpPat};
pub use branch::IfPat;
pub use call::{CallOtherPat, CallPat};
pub(crate) use cmp_op::cmp_pat;
pub use function_arg::FunctionArgPat;
pub use memory::{LoadPat, StorePat};
pub use phi::{MemPhiPat, PhiPat, ValuePhiPat};
pub use ret::RetPat;
pub(crate) use unary_op::unary_pat;
