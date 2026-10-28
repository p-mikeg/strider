#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

mod builder;
mod control_flow_view;
pub use control_flow_view::{
    CtrlKey, DominatorTree, control_dominator_tree, control_edge_dominator_tree,
};
pub(crate) mod error;
pub mod eval;
mod function;
#[cfg(any(test, feature = "test-util"))]
pub use function::cc_ret_and_clobber_vns;
pub use function::{AssumptionOptions, EditFunction, Function, MemDecomp, MemoryId, SideTables};
pub mod graph;
pub use graph::Graph;
pub mod node;
pub use graph::Inputs;
mod node_signature;
mod region;
mod schedule;
pub use ::read_only_memory::ReadOnlyMemory;
pub use schedule::early_position;
pub mod validate;
mod viewer;
pub mod walk;

#[cfg(test)]
mod test_cfgs;

pub use crate::error::Result;
pub use crate::node::const_value::ConstId;
pub use builder::{FunctionBuilder, IRBuilder, IRBuilderExt};
pub use node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp, VnTypeExt,
};
pub use region::RegionId;
pub use viewer::{IRViewer, IRWalker};

pub type ValueType = node::ValueType;
