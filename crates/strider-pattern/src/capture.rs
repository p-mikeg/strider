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
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Capture {
    id: u32,
    internal: bool,
}

// Every `Capture` is minted explicitly: a `.default()` would let a
// `#[derive(Default)]` on any containing struct silently allocate ids.
#[allow(clippy::new_without_default)]
impl Capture {
    pub fn new() -> Self {
        Self {
            id: next_id(),
            internal: false,
        }
    }

    /// Minted by a builder, never by a caller: it holds an identity the
    /// pattern graph cannot express, and is filtered out of everything a
    /// caller sees. See [`PatValue::identity`](crate::matcher::PatValue).
    pub(crate) fn internal() -> Self {
        Self {
            id: next_id(),
            internal: true,
        }
    }

    /// An opaque hash key: nothing outside this module may depend on how
    /// ids are allocated.
    pub fn id(self) -> u32 {
        self.id
    }

    pub(crate) fn is_internal(self) -> bool {
        self.internal
    }
}

// The flag is bookkeeping; error messages name a capture by its id alone.
impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Capture({})", self.id)
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
