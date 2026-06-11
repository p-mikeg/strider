//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`call_stack_args`, `load_forward`, `function_args::stack_args`).
//!
//! The implementation is split across focused submodules:
//!
//! * `decompose` — the SP-decomposer (`SpDecomposer`, `SpExpr`,
//!   `SpExprMemo`).  Constant addends are read via the canonical
//!   `IRViewer::int_const_i64`; `ConstantFold` has already collapsed any
//!   `Neg`/`Truncate`/`Extend` wrapper by the time these passes run, so the
//!   decomposer never peels those shapes itself.
//! * `ranges` — range arithmetic (`ranges_disjoint`,
//!   `store_value_byte_size`) used by every alias check.
//! * `walk` — address-alias classification (`AddrClass`,
//!   `alias_verdict`, `store_alias_verdict`) plus the pass-scoped
//!   `SpAliasCfg` that bundles the shared memo + alias knobs and exposes
//!   the memory-SSA queries (`classify_addr` / `nearest_clobber` /
//!   `reaching_store`), deciding whether a store aliases a precomputed
//!   load address class.

mod decompose;
mod ranges;
mod walk;

pub use decompose::{SpExpr, SpExprMemo};
pub use ranges::ranges_disjoint;

pub(crate) use decompose::SpDecomposer;
pub(crate) use walk::{AddrClass, AliasVerdict, SpAliasCfg, alias_verdict};
