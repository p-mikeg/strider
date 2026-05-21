//! `FunctionBuilderCC` — the thin calling-convention slice that
//! `FunctionBuilder` actually consumes.  Defined in `strider-ir` so the
//! IR crate doesn't pull a back-edge dep on `target`.
//!
//! `target` lives above `strider-ir` in the crate-dependency order, so
//! importing `target::BuiltCallingConvention` from `strider-ir` would
//! invert that ordering.  `FunctionBuilderCC` exposes only the fields
//! `FunctionBuilder::new` / `FunctionBuilder::build_call_with_cc`
//! actually read; `target` provides `impl From<BuiltCallingConvention>
//! for FunctionBuilderCC` at the layer boundary so the richer ABI type
//! stays in its home crate.
//!
//! # Field set
//!
//! Mirrors `target::BuiltCallingConvention`'s public accessors that the
//! IR builder reads.  Field names match the source type one-to-one so
//! the `From` impl is a straight field copy:
//!
//! - `arg_passing_regs` — used by `build_call_with_cc(override_cc)` to
//!   resolve per-call arg slots.
//! - `callee_saved_regs` — used by `FunctionBuilder::new` to derive the
//!   function-default clobber list and by `build_call_with_cc` to derive
//!   the per-call override clobber list.
//! - `ret_val_regs` / `ret_val_regs_float` — used by `FunctionBuilder::new`
//!   to seed the tracked-variable set with all return registers (so the
//!   Return node's data-flow chain stays connected to in-function writes
//!   even when the float ret reg is a sub-register of a wider tracked
//!   container).
//! - `stack_ptr_vn` — used by `FunctionBuilder::new` (passed to `new_raw`
//!   as the stack-pointer arg) and by `build_call_with_cc` indirectly via
//!   the `stack_ptr_vn` field stored on the builder.
//! - `ret_stack_pop` — used to drive the post-call SP-adjust node.
//! - `no_memory_clobber` — function-default memory-preservation flag for
//!   zero-side-effect hook conventions (`x86_64_all_preserving` etc.).

use rsleigh::Vn;

/// Plain-data calling-convention slice for [`crate::FunctionBuilder`].
///
/// Construct directly for synthetic/test use, or via
/// `impl From<target::BuiltCallingConvention> for FunctionBuilderCC`
/// in production code.  See the module docs for the field-by-field
/// usage contract.
#[derive(Debug, Clone)]
pub struct FunctionBuilderCC {
    /// Argument-passing register varnodes, in positional order.
    pub arg_passing_regs: Vec<Vn>,
    /// Callee-saved register varnodes (excludes SP).
    pub callee_saved_regs: Vec<Vn>,
    /// Integer return-value register varnodes, in positional order.
    pub ret_val_regs: Vec<Vn>,
    /// Float return-value register varnodes, in positional order.
    pub ret_val_regs_float: Vec<Vn>,
    /// Hardware stack-pointer varnode.
    pub stack_ptr_vn: Vn,
    /// Net byte change the callee's `ret` inflicts on the caller's SP
    /// (`8` on x86_64, `0` on link-register ISAs).
    pub ret_stack_pop: i64,
    /// True for hook-style conventions like `x86_64_all_preserving` where
    /// `Call` nodes don't advance the memory chain.
    pub no_memory_clobber: bool,
}
