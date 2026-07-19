//! Traversal and dominance-based SSA support, generic over opaque node ids.
//! No CFG/IR type is in scope here.

extern crate alloc;

pub mod dominance;
pub mod walk;
