//! Helpers shared by the SP-aware opt passes
//! ([`CallStackArgCollect`][crate::opt::CallStackArgCollect],
//! [`FunctionArgDetect`][crate::opt::FunctionArgDetect]) when they
//! synthesise a minimal calling convention from raw constructor
//! arguments.
//!
//! Tests in this crate frequently construct one of the passes with just
//! a stack-pointer varnode and a stack-arg offset list / register-arg
//! list.  Those convenience `::new(...)` constructors funnel through
//! here so the passes share a single synthetic-CC construction site
//! and a single drift surface.

use strider_target::BuiltCallingConvention;

/// Synthesises a minimal [`BuiltCallingConvention`] for the given
/// stack-pointer varnode, register-passed argument list, and
/// stack-passed argument offset list.  All other CC fields default to
/// empty / `None` / `0`.
///
/// Bypasses [`BuiltCallingConvention::try_new`] because this helper
/// has only ever called from the SP-aware passes' `::new(...)`
/// test-shorthand constructors, which must stay infallible.
/// Validation of CC-table presets happens upstream in
/// `CallingConvention::build`; callers here are not building presets.
/// The struct literal is the simplest infallible construction path.
#[must_use]
pub(crate) fn minimal_cc(
    stack_ptr_vn: rsleigh::Vn,
    arg_passing_regs: Vec<rsleigh::Vn>,
    stack_arg_offsets: Vec<i64>,
) -> BuiltCallingConvention {
    BuiltCallingConvention {
        arg_passing_regs,
        callee_saved_regs: Vec::new(),
        ret_val_regs: Vec::new(),
        ret_val_regs_float: Vec::new(),
        stack_ptr_vn,
        stack_arg_offsets,
        ret_stack_pop: 0,
        link_register_vn: None,
        no_memory_clobber: false,
    }
}
