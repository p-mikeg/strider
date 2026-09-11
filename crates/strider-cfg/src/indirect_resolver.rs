//! Classifying a `BranchIndirect` needs the IR, which sits above this crate in
//! the dependency order, so [`crate::Builder`] takes its answers from
//! `options.known_targets` and defers every other site via
//! [`crate::RegionTerminator::UnresolvedIndirectBranch`].

/// One resolved branch target.
///
/// `isa_bit` is the ISA mode the branch commits for this target on an
/// alternate-ISA arch (ARM Thumb, MIPS16): `Some(true)` alternate, `Some(false)`
/// base, `None` when the branch commits none and the target inherits the mode
/// flowing into the branch. `addr` already has the mode bit masked off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub addr: u64,
    pub isa_bit: Option<bool>,
}

impl ResolvedTarget {
    #[must_use]
    pub fn new(addr: u64, isa_bit: Option<bool>) -> Self {
        Self { addr, isa_bit }
    }
}

/// A bare address with no mode switch (`isa_bit: None`), the common case.
impl From<u64> for ResolvedTarget {
    fn from(addr: u64) -> Self {
        Self {
            addr,
            isa_bit: None,
        }
    }
}

/// The statically-known targets of one `BranchIndirect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTargets {
    /// Return via the link register (ARM/AArch64 `bx lr`).  Seated as a
    /// [`crate::RegionTerminator::Return`].
    LinkRegister,
    /// One constant target: an intra-function edge, or a tail call when out
    /// of function range.
    Single(ResolvedTarget),
    /// A jump table, seated as a [`crate::RegionTerminator::Switch`].
    /// Sorted-deduplicated by the classifier.
    ///
    /// Must be non-empty.  An empty `Multiple` advertises zero runtime
    /// targets, making the dispatch site look unreachable.
    Multiple(Vec<ResolvedTarget>),
}
