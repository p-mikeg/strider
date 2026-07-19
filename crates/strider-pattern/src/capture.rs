//! One handle type for both data and control positions. After a match,
//! `Match::node` gives the `NodeId` and `Match::value` the `ValueId`, the
//! latter `None` for control-flow nodes with no single value output.

use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Ids are globally unique via a process-wide atomic counter.
///
/// The same `Capture` may appear in several pattern positions; the matcher
/// then requires every occurrence to bind the **same** node (and the same
/// value output, where applicable).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Capture(u32);

// No `Default`: minting a fresh global id from `.default()` means a
// `#[derive(Default)]` on any containing struct silently allocates ids.
#[allow(clippy::new_without_default)]
impl Capture {
    pub fn new() -> Self {
        Self(next_id())
    }

    /// An opaque hash key. The id space is neither dense nor sequential.
    pub fn id(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_ids_are_globally_unique() {
        let n = 256;
        let ids: std::collections::HashSet<u32> = (0..n).map(|_| Capture::new().id()).collect();
        assert_eq!(ids.len(), n);
    }
}
