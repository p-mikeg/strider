#![warn(clippy::wildcard_enum_match_arm)]

mod builder;
mod control_flow_view;
pub use control_flow_view::{
    CtrlKey, DominatorTree, control_dominator_tree, control_edge_dominator_tree,
};
pub mod eval;
mod function;
#[cfg(any(test, feature = "test-util"))]
pub use function::cc_ret_and_clobber_vns;
pub use function::{
    AssumptionOptions, EditFunction, Function, FunctionSideTables, MemDecomp, MemoryId,
};
pub mod graph;
pub use graph::{Graph, Inputs};
pub mod node;
mod region;
mod schedule;
/// The int-constant type [`IRViewer::int_const`] answers and
/// [`IRBuilderExt::build_int_const`] takes, so a caller can name it, beside
/// the signed form [`IRViewer::int_const_ibig`] answers.
pub use ::dashu_int::{IBig, UBig};
pub use ::read_only_memory::ReadOnlyMemory;
pub use schedule::early_position;
pub mod validate;
mod viewer;
pub mod walk;

#[cfg(test)]
mod test_cfgs;

pub use builder::{FunctionBuilder, IRBuilder, IRBuilderExt};
pub use node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntConstId,
    IntUnaryOp, ValueType, VnTypeExt,
};
pub use region::RegionId;
pub use viewer::{IRViewer, IRWalker};

pub type Result<T> = anyhow::Result<T>;
