//! Helpers shared by the four SP-aware opt passes
//! ([`StackStoreDetect`][crate::opt::StackStoreDetect],
//! [`StackLoadForward`][crate::opt::StackLoadForward],
//! [`CallStackArgCollect`][crate::opt::CallStackArgCollect],
//! [`FunctionArgDetect`][crate::opt::FunctionArgDetect]) when they
//! synthesise a minimal calling convention from raw constructor
//! arguments.
//!
//! Tests in this crate frequently construct one of the four passes
//! with just a stack-pointer varnode (and sometimes a stack-arg offset
//! list / register-arg list).  Those convenience `::new(...)`
//! constructors funnel through here so the four passes share a single
//! synthetic-CC construction site and a single drift surface.

use strider_target::{BuiltCallingConvention, PositionalArgLayout};

/// Synthesises a minimal [`BuiltCallingConvention`] for the given
/// stack-pointer varnode.  No register-passed args, no stack-passed
/// args, no callee-saved regs, no link register.  Suitable for unit
/// tests that drive an SP-aware pass with only a stack-pointer
/// varnode.
///
/// Routes through [`BuiltCallingConvention::try_new`] so the
/// SP-not-in-any-reg-list invariant is checked at construction; that
/// check trivially passes here because every reg-list is empty.
#[must_use]
pub(crate) fn minimal_cc_for_sp(stack_ptr_vn: rsleigh::Vn) -> BuiltCallingConvention {
    minimal_cc(stack_ptr_vn, Vec::new(), Vec::new())
}

/// Synthesises a minimal [`BuiltCallingConvention`] for the given
/// stack-pointer varnode, register-passed argument list, and
/// stack-passed argument offset list.  All other CC fields default to
/// empty / `None` / `0`.
///
/// Bypasses [`BuiltCallingConvention::try_new`] because this helper
/// has only ever called from the four passes' `::new(...)`
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

/// Derives the [`PositionalArgLayout`] for a calling convention.
/// Thin wrapper to make call sites in the four passes self-documenting:
/// every pass that needs positional-arg knowledge goes through this
/// one helper so the derivation source is unambiguous.
#[must_use]
pub(crate) fn layout_of(cc: &BuiltCallingConvention) -> PositionalArgLayout {
    PositionalArgLayout::from_convention(cc)
}
