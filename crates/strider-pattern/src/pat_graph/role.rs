//! Phantom role markers + their composition rule.
//!
//! `Wildcard` means the pattern contains at least one node that cannot
//! be instantiated (kind-`Any` or a custom predicate with no build
//! spec) — matchable only.  `Concrete` means every node has either a
//! concrete `NodeKind` or a capture that resolves at template time —
//! matchable AND buildable.
//!
//! Composing two roles via `Combine` returns the weaker (Wildcard
//! absorbs Concrete).

mod sealed {
    pub trait Sealed {}
}

/// Sealed marker trait — only `Wildcard` and `Concrete` implement it.
pub trait Role: sealed::Sealed {}

/// Pattern contains a node that cannot be instantiated.  Matchable
/// only; `Template` is NOT implemented for `Pat<Wildcard>`.
pub struct Wildcard;
impl sealed::Sealed for Wildcard {}
impl Role for Wildcard {}

/// Every node in the pattern has a build path (concrete `NodeKind` or
/// `Capture`).  Matchable AND buildable.
pub struct Concrete;
impl sealed::Sealed for Concrete {}
impl Role for Concrete {}

/// Compose two roles.  Weaker role wins (Wildcard absorbs Concrete).
pub trait Combine<Other: Role>: Role {
    type Output: Role;
}

impl Combine<Wildcard> for Wildcard {
    type Output = Wildcard;
}
impl Combine<Wildcard> for Concrete {
    type Output = Wildcard;
}
impl Combine<Concrete> for Wildcard {
    type Output = Wildcard;
}
impl Combine<Concrete> for Concrete {
    type Output = Concrete;
}
