//! [`crate::Builder`] cannot classify a `BranchIndirect` itself: that needs
//! the IR, which sits above this crate in the dependency order.  Any site not
//! pre-classified in `options.known_targets` is deferred via
//! [`crate::RegionTerminator::UnresolvedIndirectBranch`], and the
//! orchestrator's rebuild loop feeds classifications back in.
//!
//! [`ResolvedTargets`] lives here rather than in strider-opt to keep that
//! feedback edge one-way; it is a pure value with no IR deps.

/// The statically-known targets of one `BranchIndirect`, produced by
/// `strider_opt::indirect_branch_resolve::classify_target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// Return via the link register (ARM/AArch64 `bx lr`).  Seated as a
    /// [`crate::RegionTerminator::Return`].
    LinkRegister,
    /// One constant target: an intra-function edge, or a tail call when out
    /// of function range.
    Single(u64),
    /// A jump table, seated as a [`crate::RegionTerminator::Switch`].
    /// Sorted-deduplicated by the classifier.
    ///
    /// Must be non-empty.  An empty `Multiple` advertises zero runtime
    /// targets, making the dispatch site look unreachable.
    Multiple(Vec<u64>),
}
