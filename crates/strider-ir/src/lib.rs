#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider IR: sea-of-nodes graph, validation, traversal.
//!
//! Sea-of-nodes intermediate representation for the Strider binary analysis
//! framework.  Generic helpers live in their own unprefixed sibling crates:
//! `dot` (Graphviz rendering), `entity-utils` (cranelift-entity helpers),
//! `graphwalk` (graph traversal).
//!
//! The IR represents a lifted function as a directed graph where each
//! [`node::NodeId`] is a computation or control-flow primitive.  Nodes have
//! typed outputs ([`node::ValueId`]) connected as inputs to downstream
//! nodes.  Structurally equal nodes with the same inputs are deduplicated and
//! cached inside [`Graph`].
//!
//! # Building the IR
//!
//! Use [`FunctionBuilder`] to construct the IR for a single function.  The
//! builder tracks SSA-like variable state per basic block and inserts
//! [`node::NodeKind::Phi`] nodes automatically at join points.
//!
//! The high-level entry point is the `strider-orchestrator` crate's
//! `orchestrator::Strider::analyze`, which feeds a per-region driver that in turn
//! drives [`FunctionBuilder`] from the p-code CFG built by `strider-lift`
//! against `rsleigh`.
//!
//! # Key types
//!
//! - [`Function`] — lifted function: [`Graph`] plus per-function state
//!   (`entry`, calling convention); produced by [`FunctionBuilder::build`] and
//!   consumed by optimizer passes and pattern queries
//! - [`Graph`] — sea-of-nodes IR store (structural state only; no entry/CC)
//! - [`FunctionBuilder`] — constructs the graph with SSA variable tracking
//! - [`RegionId`] — identifies a basic block within the function
//! - [`node::ValueType`] — integers `I1` (the 1-bit boolean)/`I8`/`I16`/`I32`/`I64`/`I80`/`I128`/`I256`/`I512`,
//!   floats `F32`/`F64`/`F80`
//! - [`IntBinaryOp`], [`IntUnaryOp`], [`IntCmpOp`], [`ExtendOp`] —
//!   operation enumerations used in node kinds (logical ops on booleans
//!   are integer ops at `I1`)

mod builder;
mod control_flow_view;
pub use control_flow_view::{control_dominators, dominates};
pub mod error;
mod function;
pub use function::{EditFunction, Function, FunctionState, SideTables};
#[cfg(any(test, feature = "test-util"))]
pub use function::{cc_ret_and_clobber_vns, largest_container_in};
pub mod graph;
pub use graph::Graph;
pub mod node;
pub use graph::Inputs;
mod node_signature;
mod region;
pub use ::read_only_memory::ReadOnlyMemory;
pub mod const_value;
pub mod validate;
mod viewer;
pub mod walk;

pub use crate::const_value::{ConstId, ConstValue};
pub use crate::error::Result;
pub use builder::{FunctionBuilder, IRBuilder, IRBuilderExt};
#[cfg(any(test, feature = "test-util"))]
pub use builder::canonicalize_tracked;
pub use node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp, VnTypeExt,
};
pub use region::RegionId;
pub use viewer::{IRViewer, IRWalker};

pub type Value = node::ValueId;
pub type ValueType = node::ValueType;
