//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`call_stack_args`, `load_forward`, `function_args::stack_args`).
//!
//! The implementation is split across focused submodules:
//!
//! * [`decompose`] — the SP-decomposer (`decompose_sp`, `SpExpr`,
//!   `SpExprMemo`) and the `int_const_signed` constant-peeling helper it
//!   consumes.
//! * [`ranges`] — range arithmetic (`ranges_disjoint`,
//!   `store_value_byte_size`) used by every alias check.
//! * [`walk`] — address-alias classification (`AddrClass`,
//!   `classify_addr`, `alias_verdict`, `store_alias_verdict`) that
//!   combines the decomposer with the range checks to decide whether a
//!   store aliases a precomputed load address class.

mod decompose;
mod ranges;
mod walk;

pub use decompose::{decompose_sp, SpExpr, SpExprMemo};
pub use ranges::ranges_disjoint;

pub(crate) use decompose::int_const_signed;
pub(crate) use walk::{
    alias_verdict, classify_addr, AddrClass, AliasVerdict, SpAliasOracle,
};
