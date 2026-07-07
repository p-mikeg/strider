//! Dense entity-set + worklist + interner data structures.
//!
//! Generic helpers over `cranelift-entity::EntityRef` and
//! `cranelift-bitset::CompoundBitSet`.

extern crate alloc;

pub mod interner;
pub mod set;
pub mod union_dag;
pub mod worklist;

pub use interner::EntityInterner;
pub use set::DenseEntitySet;
pub use union_dag::UnionDag;
pub use worklist::Worklist;
