//! Capture variable used throughout the pattern engine.
//!
//! [`Capture`] is the unified data/control capture handle: every
//! pattern position that wants to bind a matched node uses the same
//! type.  After a successful match, [`crate::pattern::Match::node`] returns the
//! `NodeId` and [`crate::pattern::Match::output`] returns the value
//! `NodeOutputId` (or `None` for control-flow nodes that have no
//! single value output).
//!
//! Typed extraction of constant values and op-variant discriminants
//! happens after the match through [`crate::pattern::Match`] / [`crate::pattern::Bindings`]
//! helpers (`get_uint`, `get_int_binary_op`, …) which look up the bound
//! `NodeId` and inspect the underlying `NodeKind`.

use std::sync::atomic::{AtomicU32, Ordering};

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
/// [`Bindings`](crate::pattern::Bindings) storage (an append-only `Vec`)
/// identify entries unambiguously without per-pattern bookkeeping.
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
    /// Exposed for downstream consumers (e.g. PyO3 bindings) that need a
    /// stable hash key.  The raw id is meant only as an *opaque identifier*;
    /// callers must not rely on the value space being dense or sequential.
    #[must_use]
    pub fn id(self) -> u32 {
        self.0
    }

}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

// ── OffsetCapture ─────────────────────────────────────────────────────────────

/// Capture variable that binds an `i64` stack offset rather than a node id.
///
/// Used with [`crate::pattern::LoadPat::offset_capture`] and
/// [`crate::pattern::StorePat::offset_capture`] to record the SP-relative
/// offset of a matched Load/Store in a [`crate::pattern::Match`].  After a
/// successful match, retrieve the bound value via
/// [`crate::pattern::Match::captured_offset`].
///
/// Each `OffsetCapture::new()` call produces a globally unique id via the
/// same process-wide atomic counter used by [`Capture`].  Uniqueness lets the
/// matcher's offset-capture map identify entries unambiguously.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct OffsetCapture(u32);

impl OffsetCapture {
    #[must_use]
    pub fn new() -> Self {
        Self(next_id())
    }

    /// Returns the globally-unique numeric id of this capture.
    ///
    /// Exposed for downstream consumers (e.g. PyO3 bindings) that need a
    /// stable hash key.  The raw id is meant only as an *opaque identifier*;
    /// callers must not rely on the value space being dense or sequential.
    #[must_use]
    pub fn id(self) -> u32 {
        self.0
    }
}

impl Default for OffsetCapture {
    fn default() -> Self {
        Self::new()
    }
}
