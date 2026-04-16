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
//! The high-level entry point is the `analyzer` crate, which drives
//! [`FunctionBuilder`] from a p-code CFG produced by `rsleigh`.
//!
//! # Key types
//!
//! - [`node::Graph`] — raw node/edge store
//! - [`FunctionBuilder`] — constructs the graph with SSA variable tracking
//! - [`BuiltFunctionGraph`] — a finished, immutable function graph ready for
//!   optimization and querying
//! - [`RegionId`] — identifies a basic block within the function
//! - [`node::NodeOutputType`] — `Bool`, `U8`, `U16`, `U32`, or `U64`
//! - [`IntBinaryOp`], [`IntUnaryOp`], [`IntCmpOp`], [`BoolBinaryOp`],
//!   [`BoolUnaryOp`], [`ExtendOp`] — operation enumerations used in node kinds

mod builder;
mod dot;
pub mod error;
mod function;
mod graph;
pub mod node;
mod ops;
mod region;
pub mod walk;
// mod node_view;
mod iterators;
mod node_signature;
pub mod validate;

pub use crate::error::{Error, ErrorKind, Result};
pub use builder::FunctionBuilder;
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
