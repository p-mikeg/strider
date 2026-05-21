//! Stack-pointer expression decomposition shared by every SP-aware pass
//! (`stack_store::detect`, `stack_load_forward`, `function_args::stack_args`).
//!
//! The implementation is split across focused submodules:
//!
//! * [`decompose`] — the SP-decomposer (`decompose_sp`, `SpExpr`,
//!   `SpExprMemo`) and the `int_const_signed` constant-peeling helper it
//!   consumes.
//! * [`ranges`] — range arithmetic (`ranges_disjoint`,
//!   `store_value_byte_size`) used by every alias check.
//! * [`walk`] — step-through walkers (`step_through_stack_store`,
//!   `step_through_stack_store_phi`, `step_through_store`) that combine
//!   the decomposer with the range checks to decide whether a single
//!   memory-side-effecting node aliases a query byte range.

mod decompose;
mod ranges;
mod walk;

pub use decompose::{decompose_sp, SpExpr, SpExprMemo};
pub use ranges::ranges_disjoint;

pub(crate) use decompose::int_const_signed;
pub(crate) use walk::{
    step_through_stack_store, step_through_stack_store_phi, step_through_store, AliasStep,
};
