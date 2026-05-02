//! Typed errors raised by [`crate::run`].
//!
//! All strider errors propagate as [`anyhow::Error`].  When a caller
//! needs to react to a specific failure mode (rather than treat the
//! error as opaque), the typed error structs in this module can be
//! recovered via [`anyhow::Error::downcast_ref`]:
//!
//! ```ignore
//! match strider::run(config) {
//!     Ok(graph) => …,
//!     Err(e) => match e.downcast_ref::<strider::UnresolvedIndirectBranch>() {
//!         Some(_) => /* selectively skip / log */,
//!         None    => return Err(e),
//!     },
//! }
//! ```
//!
//! The Python wrapper turns each typed error into a corresponding
//! `strider.errors.*Error` subclass of `StriderError`, so Python
//! callers get the same selectivity through `try` / `except`.

use cfg::PcodeInsnAddr;

/// The orchestrator's indirect-branch fixed-point loop terminated with
/// at least one branch still unresolved.  `addr` is the program point
/// of the first such branch reported.
///
/// Typically caused by a target whose value is not statically
/// recoverable from the IR (e.g. it depends on a runtime-only register
/// like a function-table base pointer the analyser cannot prove
/// constant).  Callers that scan many functions usually want to log
/// and skip these rather than treat them as hard failures.
#[derive(Debug, thiserror::Error)]
#[error("indirect branch at {addr:?} could not be resolved at fixed point")]
pub struct UnresolvedIndirectBranch {
    pub addr: PcodeInsnAddr,
}
