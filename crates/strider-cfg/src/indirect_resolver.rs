//! [`crate::Builder`] cannot classify a `BranchIndirect` itself: that needs
//! the IR, which sits above this crate in the dependency order.  Any site not
//! pre-classified in `options.known_targets` is deferred via
//! [`crate::RegionTerminator::UnresolvedIndirectBranch`].

/// The statically-known targets of one `BranchIndirect`.
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
