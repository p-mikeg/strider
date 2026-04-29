#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Sea-of-nodes intermediate representation for the Strider binary analysis
//! framework.
//!
//! The IR represents a lifted function as a directed graph where each
//! [`node::NodeId`] is a computation or control-flow primitive.  Nodes have
//! typed outputs ([`node::NodeOutputId`]) connected as inputs to downstream
//! nodes.  Structurally equal nodes with the same inputs are deduplicated and
//! cached inside [`node::Graph`].
//!
//! # Building the IR
//!
//! Use [`FunctionBuilder`] to construct the IR for a single function.  The
//! builder tracks SSA-like variable state per basic block and inserts
//! [`node::NodeKind::ControlPhi`] nodes automatically at join points.
//!
//! The high-level entry point is the `strider` crate, which drives
//! [`FunctionBuilder`] from a p-code CFG produced by `rsleigh`.
//!
//! # Key types
//!
//! - [`node::Graph`] — raw node/edge store
//! - [`FunctionBuilder`] — constructs the graph with SSA variable tracking
//! - [`BuiltFunctionGraph`] — a finished, immutable function graph ready for
//!   optimization and querying
//! - [`RegionId`] — identifies a basic block within the function
//! - [`node::NodeOutputType`] — `Bool`, integers `U8`/`U16`/`U32`/`U64`/`U128`/`U256`,
//!   floats `F32`/`F64`
//! - [`IntBinaryOp`], [`IntUnaryOp`], [`IntCmpOp`], [`BoolBinaryOp`],
//!   [`BoolUnaryOp`], [`ExtendOp`] — operation enumerations used in node kinds

mod builder;
mod dot;
pub mod error;
mod function;
pub mod graph;
pub use graph::Graph;
pub mod node;
mod iterators;
mod node_signature;
mod ops;
mod region;
pub mod validate;
pub mod walk;

pub use crate::error::{Error, ErrorKind, Result};
pub use builder::{FunctionBuilder, VarId};
pub use node::PcodeInsnAddr;
pub use node_signature::ExpectedOutputKind;
pub use ops::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
pub use region::RegionId;
pub use validate::{ValidationError, ValidationErrors};

pub type Value = node::NodeOutputId;
pub type ValueType = node::NodeOutputType;
pub use crate::function::BuiltFunctionGraph;
