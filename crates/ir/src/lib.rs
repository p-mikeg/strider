mod dot;
mod graph;
mod builder;
pub mod node;
mod walk;
mod error;
mod function;
mod region;
mod ops;
// mod node_view;
mod iterators;

pub use crate::error::{Error, Result};
pub use builder::{FunctionBuilder};
pub use region::{RegionId};
pub use ops::{
    BoolBinaryOp, BoolUnaryOp, IntBinaryOp, IntUnaryOp, IntCmpOp, ExtendOp
};

pub type Value = node::NodeOutputId;
pub type ValueType = node::NodeOutputType;
pub use crate::function::BuiltFunctionGraph;