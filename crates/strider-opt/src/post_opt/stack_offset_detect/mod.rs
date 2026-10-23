//! Stamps the side-table memory class with the slot `(base, K)` for every Store
//! / Load whose address decomposes to a single `base + K` terminal.  `base` is
//! SP-derived, or the return of an allocator the function's claims list.
//!
//! `K` is only comparable against another access sharing the same `base`.

use crate::error::Result;
use crate::pipeline::PostOptimizer;

/// Annotates `Store` / `Load` offsets against their stack or heap base in the
/// side-table memory class.
#[derive(Clone)]
pub struct StackOffsetDetect;

impl PostOptimizer for StackOffsetDetect {
    fn apply(&self, edit: &mut crate::EditFunction<'_>, _ctx: &crate::OptCtx<'_>) -> Result<()> {
        crate::mem_analysis::stamp_all(edit);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
