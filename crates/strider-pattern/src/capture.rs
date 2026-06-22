//! Capture variables for pattern matching.
//!
//! [`Capture`] is the unified data/control capture handle: every
//! pattern position that wants to bind a matched node uses the same
//! type.  After a successful match, `Match::node` returns the
//! `NodeId` and `Match::value` returns the value
//! `ValueId` (or `None` for control-flow nodes that have no
//! single value output).

use std::sync::atomic::{AtomicU32, Ordering};

// ── Capture ──────────────────────────────────────────────────────────────────

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Unified capture variable.  Binds to a single matched node — every
/// successful match records both the node's `NodeId` and (when the
/// pattern is value-producing) the value `ValueId`.
///
/// Each `Capture::new()` call produces a globally unique id via a
/// process-wide atomic counter; uniqueness lets the matcher's
/// [`crate::Bindings`] storage (an append-only `Vec`) identify entries
/// unambiguously without per-pattern bookkeeping.
///
/// The same `Capture` can appear in multiple positions of a pattern;
/// the matcher requires all occurrences to bind to the **same** node
/// (and the same value output, if applicable).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Capture(u32);

// `new()` intentionally does NOT have a `Default` implementation: a
// `Default` that mints a globally-unique id on every `.default()` call
// is a hazard (a `#[derive(Default)]` on a containing struct silently
// allocates ids).  Suppress the lint here.
#[allow(clippy::new_without_default)]
impl Capture {
    pub fn new() -> Self {
        Self(next_id())
    }

    /// Returns the globally-unique numeric id of this capture.
    ///
    /// Exposed for downstream consumers (e.g. PyO3 bindings) that need
    /// a stable hash key.  The raw id is meant only as an *opaque
    /// identifier*; callers must not rely on the value space being
    /// dense or sequential.
    pub fn id(self) -> u32 {
        self.0
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `Capture::new()` uses a process-wide atomic counter; allocating
    /// many must produce all-distinct IDs.
    #[test]
    fn capture_ids_are_globally_unique() {
        let n = 256;
        let ids: std::collections::HashSet<u32> = (0..n).map(|_| Capture::new().id()).collect();
        assert_eq!(ids.len(), n);
    }
}
