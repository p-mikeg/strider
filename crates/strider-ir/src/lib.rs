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
    CtrlKey, control_dominators, control_edge_dominators, dominance_verdict, dominates,
};
pub mod error;
mod function;
#[cfg(any(test, feature = "test-util"))]
pub use function::cc_ret_and_clobber_vns;
pub use function::{EditFunction, Function, FunctionState, MemDecomp, MemoryId, SideTables};
pub mod graph;
pub use graph::Graph;
pub mod node;
pub use graph::Inputs;
mod node_signature;
mod region;
pub use ::read_only_memory::ReadOnlyMemory;
pub mod validate;
mod viewer;
pub mod walk;

pub use crate::error::Result;
pub use crate::node::const_value::ConstId;
pub use builder::{FunctionBuilder, IRBuilder, IRBuilderExt};
pub use node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp, VnTypeExt,
};
pub use region::RegionId;
pub use viewer::{IRViewer, IRWalker};

pub type Value = node::ValueId;
pub type ValueType = node::ValueType;
