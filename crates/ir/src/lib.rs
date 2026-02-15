mod builder_ext;
mod dot;
mod graph;
mod builder;
pub mod node;
mod walk;
mod error;
// mod node_view;
mod iterators;

pub use crate::error::{Error, Result};
pub use builder::{FunctionBuilder, BlockId};
pub use builder_ext::{
    builder::Builder,
    FunctionBody,
    bool::{BoolBinaryOpKind, BoolUnaryOpKind},
    BoolBuilderExt, IntBuilderExt, MemoryBuilderExt, ControlBuilderExt
};

pub type Value = node::NodeOutputId;
pub type ValueType = node::NodeOutputType;