//! Capture variables for pattern matching.
//!
//! [`Capture`] is the unified data/control capture handle: every
//! pattern position that wants to bind a matched node uses the same
//! type.  After a successful match, [`crate::Match::node`] returns the
//! `NodeId` and [`crate::Match::output`] returns the value
//! `NodeOutputId` (or `None` for control-flow nodes that have no
//! single value output).

use std::sync::atomic::{AtomicU32, Ordering};

// ── Capture ──────────────────────────────────────────────────────────────────

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Unified capture variable.  Binds to a single matched node — every
/// successful match records both the node's `NodeId` and (when the
/// pattern is value-producing) the value `NodeOutputId`.
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

impl Capture {
    #[must_use]
    pub fn new() -> Self {
        Self(next_id())
    }

    /// Returns the globally-unique numeric id of this capture.
    ///
    /// Exposed for downstream consumers (e.g. PyO3 bindings) that need
    /// a stable hash key.  The raw id is meant only as an *opaque
    /// identifier*; callers must not rely on the value space being
    /// dense or sequential.
    #[must_use]
    pub fn id(self) -> u32 {
        self.0
    }

    /// Intern (or look up) the [`Capture`] associated with the given
    /// `name`.  Two calls with equal `name` strings return the same
    /// `Capture`, so the same name across pat positions enforces
    /// capture-equality in the matcher.
    ///
    /// Backed by a process-wide table guarded by a mutex; intern hits
    /// are cheap O(1) hashmap lookups.  The table is append-only —
    /// captures interned this way share the same id space as
    /// [`Capture::new`] (both pull from the same atomic counter at
    /// first-time interning).
    #[must_use]
    pub fn named(name: &str) -> Self {
        use std::collections::HashMap;
        use std::sync::Mutex;
        static TABLE: std::sync::OnceLock<Mutex<HashMap<String, Capture>>> =
            std::sync::OnceLock::new();
        let table = TABLE.get_or_init(|| Mutex::new(HashMap::new()));
        // Lock-poisoning here means a prior call panicked while holding
        // the table — recover the inner map and continue (the worst-case
        // outcome is allocating a duplicate capture for a re-entry).
        let mut t = match table.lock() {
            Ok(t) => t,
            Err(poisoned) => poisoned.into_inner(),
        };
        *t.entry(name.to_string()).or_insert_with(Capture::new)
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `Capture::new()` uses a process-wide atomic counter; allocating
    /// many must produce all-distinct IDs.  `Debug` output is the only
    /// public handle on the raw ID, so the test uses it as a set key.
    #[test]
    fn capture_ids_are_globally_unique_across_many_allocations() {
        const N: usize = 256;
        let mut ids: Vec<String> = Vec::with_capacity(N);
        for _ in 0..N {
            ids.push(format!("{:?}", Capture::new()));
        }
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
