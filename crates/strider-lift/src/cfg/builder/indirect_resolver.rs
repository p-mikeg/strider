//! Indirect-branch target resolver callback.
//!
//! [`crate::cfg::Builder`] does not itself know how to classify a
//! `BranchIndirect`'s target — that knowledge lives above strider-lift
//! in the crate-dependency order.  Instead, the builder accepts an
//! installed [`IndirectResolverFn`] callback (via
//! [`crate::cfg::Builder::with_indirect_resolver`]) and delegates target
//! classification to it.
//!
//! When no resolver is installed, the cfg builder treats every
//! `BranchIndirect` as unresolvable and defers the site via
//! [`crate::cfg::RegionTerminator::UnresolvedIndirectBranch`].
//!
//! This module also owns the [`ResolvedTargets`] result enum returned by
//! both cfg-time and IR-level resolvers.  Keeping it here breaks a
//! potential dep cycle (cfg → opt for `ResolvedTargets`): the type is a
//! pure value with no IR / opt dependencies.

use strider_ir::ReadOnlyMemory;

use crate::cfg::Result;
use crate::cfg::types::RegionInstruction;
use strider_target::Endianness;

/// The set of statically-known targets of a single `BranchIndirect`.
///
/// Returned by both the cfg-time mini-IR resolver and the IR-level
/// resolver in `strider_opt::indirect_branch_resolve::classify_anchor`.
///
/// ## Variants
///
/// - [`Self::LinkRegister`] — the indirect branch is a return-via-LR
///   (typical on ARM/AArch64 with `bx lr`).  In-place edit: append the
///   ABI ret-val regs to the placeholder Return and we're done.
/// - [`Self::Single`] — the indirect branch resolves to exactly one
///   constant target.  In-place edit possible iff the target is a
///   tail call (out of function range); otherwise the orchestrator
///   does a CFG rebuild.
/// - [`Self::Multiple`] — the indirect branch resolves to a known set
///   of constant targets (jump table).  Always requires a CFG rebuild;
///   the orchestrator handles these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// The indirect branch dispatches to the link register's
    /// caller-provided value (i.e. a function return via LR).
    LinkRegister,
    /// The indirect branch resolves to exactly one constant target.
    Single(u64),
    /// The indirect branch resolves to a known set of constant
    /// targets.  Sorted-deduplicated by the classifier.
    ///
    /// **Invariant:** the inner `Vec` must be **non-empty**.  An
    /// empty `Multiple` would silently advertise zero runtime targets,
    /// making the dispatch site appear unreachable.  Callers must
    /// establish non-emptiness before constructing this variant.
    Multiple(Vec<u64>),
}

/// Callback invoked by [`crate::cfg::Builder`] when it encounters a
/// `BranchIndirect` whose target is not pre-classified (via
/// `with_known_targets`).  The cfg builder hands the callback the
/// region's accumulated pcode plus the dispatch varnode and lets it
/// decide whether the target is a constant, a link-register read, or
/// unresolvable.
///
/// `region_insns` is the current region's pcode instructions in
/// program order, **including the trailing `BranchIndirect`**.
/// `target_vn` is the dispatch varnode (`BranchIndirect`'s
/// `inputs[0]`).  `sleigh` is the active Sleigh context used to drive
/// any IR lifting the resolver needs.  `cc_link_register_vn` is the
/// calling convention's link-register varnode (`Some` on link-register
/// ISAs, `None` on stack-push ISAs like x86/x86_64).  `rom` is the
/// binary's read-only memory image consulted when folding
/// constant-address loads (e.g. rodata-resident jump tables).
/// `endianness` drives byte-order for the resolver's internal lifter.
///
/// Returns `Ok(Some(targets))` on a successful classification,
/// `Ok(None)` when the target is not statically recoverable from the
/// region's pcode alone (the caller defers via
/// [`crate::cfg::RegionTerminator::UnresolvedIndirectBranch`]), and
/// `Err` on internal errors (malformed pcode, opt failures).
///
/// The resolver is single-owner (`Box<dyn Fn>`): strider runs
/// single-threaded, so `Arc` sharing is not required.
pub type IndirectResolverFn<R> = Box<
    dyn Fn(
            &[RegionInstruction],
            rsleigh::Vn,
            &rsleigh::Sleigh<R>,
            Option<rsleigh::Vn>,
            Option<&dyn ReadOnlyMemory>,
            Endianness,
        ) -> Result<Option<ResolvedTargets>>,
>;
