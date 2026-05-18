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
//! v2-rewrite consolidation crate.  Absorbs `entity-utils`, `graphwalk`,
//! `dot`, `graphmock`, and the former `ir` crate.  See
//! `docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md` Phase 1 for the
//! migration plan.
//!
//! Sea-of-nodes intermediate representation for the Strider binary analysis
//! framework.
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
//! [`node::NodeKind::VarPhi`] nodes automatically at join points.
//!
//! The high-level entry point is the `strider` crate, which drives
//! [`FunctionBuilder`] from a p-code CFG produced by `rsleigh`.
//!
//! # Key types
//!
//! - [`Graph`] — raw node/edge store
//! - [`FunctionBuilder`] — constructs the graph with SSA variable tracking
//! - [`BuiltFunctionGraph`] — a finished, immutable function graph ready for
//!   optimization and querying
//! - [`RegionId`] — identifies a basic block within the function
//! - [`node::NodeOutputType`] — `Bool`, integers `U8`/`U16`/`U32`/`U64`/`U80`/`U128`/`U256`/`U512`,
//!   floats `F32`/`F64`/`F80`
//! - [`IntBinaryOp`], [`IntUnaryOp`], [`IntCmpOp`], [`BoolBinaryOp`],
//!   [`BoolUnaryOp`], [`ExtendOp`] — operation enumerations used in node kinds

extern crate alloc;

// Absorbed utility crates (Phase 1 Tasks 1.1 + 1.2).
pub mod dot;
pub mod entity_utils;
pub mod graphmock;
pub mod graphwalk;

// Absorbed ir crate (Phase 1 Task 1.3).
mod builder;
pub mod error;
mod function;
pub mod graph;
pub use graph::Graph;
/// IR-specific Graphviz/dot rendering (was `ir::dot` before Task 1.3 —
/// renamed to `graph_dot` to disambiguate from the generic [`dot`] module
/// absorbed in Task 1.2).  The `ir` shim re-exports this as `ir::dot` for
/// backwards compatibility.
pub mod graph_dot;
pub mod node;
mod iterators;
mod node_signature;
mod ops;
mod region;
pub mod read_only_memory;
pub use read_only_memory::ReadOnlyMemory;
#[cfg(any(feature = "test-utils", test))]
pub mod test_helpers;
#[cfg(any(feature = "test-utils", test))]
pub mod test_utils;
pub mod validate;
pub mod walk;
pub mod wide_const;
pub use wide_const::{WideConstId, WideConstStorage};

pub use crate::error::Result;
pub use builder::{FunctionBuilder, VarId};
pub use ops::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};
pub use region::RegionId;
pub use validate::{ValidateOptions, ValidationError, ValidationErrors};

pub type Value = node::NodeOutputId;
pub type ValueType = node::NodeOutputType;
pub use crate::function::BuiltFunctionGraph;
