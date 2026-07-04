//! Generic graph algorithms: traversal (pre/post-order) plus dominance-based
//! SSA support (dominance frontiers, dominator-tree preorder, iterated-DF φ
//! placement) — all parameterised over opaque node ids, so no CFG/IR type is
//! in scope here.

extern crate alloc;

pub mod dominance;
pub mod walk;
