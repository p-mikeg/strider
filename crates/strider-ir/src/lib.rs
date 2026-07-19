#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Sea-of-nodes IR: a lifted function as a bipartite graph of
//! [`node::NodeId`] computations producing typed [`node::ValueId`] outputs.
//! Structurally equal cacheable nodes with equal inputs are deduplicated
//! inside [`Graph`].
//!
//! [`Graph`] holds structural state only; per-function overlay state (entry,
//! calling convention, side-tables) lives on [`Function`].
//!
//! Access is split across four traits, since a reader cannot tell them apart
//! from the method names alone:
//!
//! - [`IRViewer`]: point reads, one required method (`function()`); everything
//!   else is a default method.
//! - [`IRWalker`]: control-aware walks, blanket-impl'd over every `IRViewer`.
//!   [`EditFunction`] shadows the order-producing methods to reuse its cached
//!   live/roots bookkeeping instead of re-walking from entry.
//! - [`IRBuilder`]: the node-creation seam.
//! - [`IRBuilderExt`]: the blanket `build_*` construction vocabulary.
//!
//! Booleans are the 1-bit integer `I1`; there is no separate bool type or
//! bool-specific op family.

mod builder;
mod control_flow_view;
pub use control_flow_view::{CtrlKey, control_dominators, control_edge_dominators, dominates};
pub mod error;
mod function;
#[cfg(any(test, feature = "test-util"))]
pub use function::cc_ret_and_clobber_vns;
pub use function::{EditFunction, Function, FunctionState, SideTables, SpDecomp, StackId};
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
pub use crate::node::const_value::{ConstId, ConstValue};
pub use builder::{FunctionBuilder, IRBuilder, IRBuilderExt};
pub use node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp, VnTypeExt,
};
pub use region::RegionId;
pub use viewer::{IRViewer, IRWalker};

pub type Value = node::ValueId;
pub type ValueType = node::ValueType;
