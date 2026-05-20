//! Dense entity-set + worklist data structures.
//!
//! Generic helpers over `cranelift-entity::EntityRef` and
//! `cranelift-bitset::CompoundBitSet`.

extern crate alloc;

pub mod set;
pub mod worklist;

pub use set::DenseEntitySet;
pub use worklist::Worklist;
