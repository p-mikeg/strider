use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A process-unique capture handle. Every position it appears at must bind the same node (and
/// value, where applicable).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Capture {
    id: u32,
    internal: bool,
}

// No `Default`: a derived one on a containing struct would silently mint ids.
#[allow(clippy::new_without_default)]
impl Capture {
    pub fn new() -> Self {
        Self {
            id: next_id(),
            internal: false,
        }
    }

    /// A builder-minted capture, hidden from callers; see
    /// [`PatValue::identity`](crate::matcher::PatValue).
    pub(crate) fn internal() -> Self {
        Self {
            id: next_id(),
            internal: true,
        }
    }

    /// Counts up from 0; the top of the range is reserved for join-dedup sentinels.
    pub fn id(self) -> u32 {
        self.id
    }

    pub(crate) fn is_internal(self) -> bool {
        self.internal
    }
}

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
