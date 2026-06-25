//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`call_stack_args`, `load_forward`, `function_args::stack_args`).
//!
//! The implementation is split across focused submodules:
//!
//! * `analyzer` — the merged `SpAnalyzer` (SP decomposition + address
//!   classification + store-alias verdict), plus `SpExpr`, `SpExprMemo`, the
//!   `AddrClass` taxonomy, and the pure class-on-class `alias_verdict` table.
//!   Constant addends are read via the canonical `IRViewer::int_const_i64`;
//!   `ConstantFold` has already collapsed any `Neg`/`Truncate`/`Extend` wrapper
//!   by the time these passes run, so the decomposer never peels those shapes
//!   itself.
//! * `ranges` — range arithmetic (`ranges_disjoint`,
//!   `store_value_byte_size`) used by every alias check.
//! * `cfg` — the pass-scoped `SpAliasCfg` façade that bundles the shared memo +
//!   alias knobs and exposes the memory-SSA queries (`classify_addr` /
//!   `nearest_clobber` / `reaching_store`), driving `mem_ssa` with the
//!   `analyzer` verdicts to decide whether a store aliases a precomputed load
//!   class.
//! * `mem_ssa` — the payload-agnostic backward memory-SSA walk
//!   (`MemorySSAWalker` oracle trait + DFS engine) that `cfg` drives; its sole
//!   production consumer is this module.

mod analyzer;
mod cfg;
mod mem_ssa;
mod ranges;

pub(crate) use analyzer::{AliasVerdict, SpExpr, SpExprMemo};
pub(crate) use cfg::SpAliasCfg;
pub(crate) use mem_ssa::narrow_load_to;
pub(crate) use ranges::high_low_shift_bits;
