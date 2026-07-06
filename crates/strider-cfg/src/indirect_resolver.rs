//! [`ResolvedTargets`] — the statically-known target set of a single
//! `BranchIndirect`.
//!
//! [`crate::Builder`] does not itself know how to classify a
//! `BranchIndirect`'s target — that knowledge lives above strider-lift
//! in the crate-dependency order.  The cfg builder treats every
//! `BranchIndirect` not pre-classified in `options.known_targets` as
//! unresolvable and defers the site via
//! [`crate::RegionTerminator::UnresolvedIndirectBranch`]; the
//! orchestrator's rebuild-driven loop classifies it against the
//! optimised IR and feeds the result back via
//! [`crate::CfgOptions::known_targets`].
//!
//! This module owns the [`ResolvedTargets`] result enum produced by the
//! IR-level resolver.  Keeping it here breaks a potential dep cycle
//! (cfg → opt for `ResolvedTargets`): the type is a pure value with no
//! IR / opt dependencies.

/// The set of statically-known targets of a single `BranchIndirect`.
///
/// Produced by the IR-level resolver in
/// `strider_opt::indirect_branch_resolve::classify_target` and fed back
/// into the cfg build via [`crate::CfgOptions::known_targets`].
///
/// ## Variants
///
/// - [`Self::LinkRegister`] — the indirect branch is a return-via-LR
///   (typical on ARM/AArch64 with `bx lr`).  The cfg seats it as a
///   [`crate::RegionTerminator::Return`].
/// - [`Self::Single`] — the indirect branch resolves to exactly one
///   constant target (an intra-function edge, or a tail call when the
///   target is out of function range).
/// - [`Self::Multiple`] — the indirect branch resolves to a known set
///   of constant targets (jump table); the cfg seats it as a
///   [`crate::RegionTerminator::Switch`].
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
