//! Re-exports for the cast-walk-through primitives.  The bitflags type and
//! the `cast_mask_of` classifier live in `strider_ir::walk` — they are pure
//! structural classification over `NodeKind` and belong with the other
//! traversal primitives.  This thin re-export preserves the historical
//! import paths used by the matcher implementation and downstream tests.

pub use strider_ir::walk::{cast_mask_of, CastMask};
