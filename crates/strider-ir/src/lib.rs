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
//! typed outputs ([`node::NodeOutputId`]) connected as inputs to downstream
//! nodes.  Structurally equal nodes with the same inputs are deduplicated and
//! cached inside [`Graph`].
//!
//! # Building the IR
//!
//! Use [`FunctionBuilder`] to construct the IR for a single function.  The
//! builder tracks SSA-like variable state per basic block and inserts
//! [`node::NodeKind::Phi`] nodes automatically at join points.
//!
//! The high-level entry point is the `strider-analyze` crate, which
//! drives [`FunctionBuilder`] from a p-code CFG produced by `rsleigh`.
//!
//! # Key types
//!
//! - [`Function`] — lifted function: [`Graph`] plus per-function state
//!   (`entry`, `cc_metadata`); produced by [`FunctionBuilder::build`] and
//!   consumed by optimizer passes and pattern queries
//! - [`Graph`] — sea-of-nodes IR store (structural state only; no entry/CC)
//! - [`FunctionBuilder`] — constructs the graph with SSA variable tracking
//! - [`RegionId`] — identifies a basic block within the function
//! - [`node::NodeOutputType`] — `Bool`, integers `U8`/`U16`/`U32`/`U64`/`U80`/`U128`/`U256`/`U512`,
//!   floats `F32`/`F64`/`F80`
//! - [`IntBinaryOp`], [`IntUnaryOp`], [`IntCmpOp`], [`BoolBinaryOp`],
//!   [`BoolUnaryOp`], [`ExtendOp`] — operation enumerations used in node kinds

mod builder;
pub mod error;
mod function;
pub use function::Function;
pub mod graph;
pub use graph::Graph;
pub mod mem_partition;
pub use mem_partition::{AliasClass, MemPartitionId, PartitionInfo, PartitionTable};
/// IR-specific Graphviz/dot rendering (implements the [`dot::GraphDotDumper`]
/// trait for the IR [`Graph`]).  Internal: external callers should use
/// [`Graph::dot_dumper`] instead of naming this module directly.
pub(crate) mod graph_dot;
pub mod node;
mod iterators;
mod node_signature;
mod ops;
mod region;
pub mod read_only_memory;
pub use read_only_memory::ReadOnlyMemory;
pub mod validate;
pub mod walk;
pub mod wide_const;

pub use crate::error::Result;
pub use builder::FunctionBuilder;
pub use ops::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
pub use region::RegionId;

pub type Value = node::NodeOutputId;
pub type ValueType = node::NodeOutputType;
