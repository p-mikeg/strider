//! Sleigh-decoded `LiftRes` cache shared across CFG rebuilds.
//!
//! During the strider orchestrator's indirect-branch fixed-point, the
//! `Cfg` is rebuilt every iteration that resolves a new target.  Each
//! rebuild lifts every byte through Sleigh, paying for every machine
//! instruction's decode again — even though the bytes are identical
//! and the Sleigh context is the same.
//!
//! This cache lets callers persist `(machine_addr) → Arc<LiftRes>`
//! across rebuilds.  Key invariants:
//!
//! 1. **Sleigh-context scoped.**  The same machine code lifted by
//!    *different* `Sleigh` contexts can produce different `LiftRes`
//!    objects (Sleigh embeds pointer-derived offsets — see the
//!    docstring on `rsleigh::LiftRes`).  A single cache instance
//!    must therefore stay tied to one Sleigh handle for its
//!    lifetime; users construct it alongside their Sleigh and
//!    abandon it when they abandon the Sleigh.
//! 2. **Read-only.**  `LiftRes` is not mutated post-decode, so the
//!    cached `Arc` can be cheaply cloned by every consumer of an
//!    address.
//!
//! The orchestrator constructs one `DecodeCache` at the top of
//! `strider::run`, threads it into every `cfg::Builder` via
//! [`crate::cfg::Builder::with_decode_cache`], and discards it when the
//! `run` returns.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rsleigh::LiftRes;
use rustc_hash::FxHashMap;

/// Shared-ownership cache from machine address to Sleigh's lifted
/// pcode.  Cheap to clone (single `Arc::clone`).
// TODO: remove after incremental indirect-resolve lands —
// see docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md
#[derive(Clone, Default)]
pub struct DecodeCache {
    inner: Arc<Mutex<FxHashMap<u64, Arc<LiftRes>>>>,
    /// Total number of `get`-or-`insert` lift-call lookups served by
    /// this cache instance — both cache hits and cache misses count.
    /// `RegionBuilder::lift_one_cached` calls `get` exactly once per
    /// machine-instruction decode attempt; we increment this counter
    /// from `get` so the metric covers every consumer.  The
    /// `Lifter::decode_stats()` facade reads this value via
    /// [`Self::total_lift_calls`].
    total_lift_calls: Arc<AtomicUsize>,
}

impl DecodeCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FxHashMap::default())),
            total_lift_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the cached `LiftRes` for `addr` if one has been
    /// recorded, else `None`.
    ///
    /// Mutex poisoning recovers via `into_inner` — a previous panic
    /// while holding the lock leaves the map intact, and a stale
    /// entry can't miscompile (the next decode will either hit the
    /// cache and return the same `LiftRes` or miss and recompute).
    #[must_use]
    pub fn get(&self, addr: u64) -> Option<Arc<LiftRes>> {
        // Bump the total-lookup counter on every `get` so the Lifter
        // facade can report `(unique_addresses, total_lift_calls)`.
        self.total_lift_calls.fetch_add(1, Ordering::Relaxed);
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&addr).cloned()
    }

    /// Records `lift_res` for `addr`.  Replaces any prior entry.
    pub fn insert(&self, addr: u64, lift_res: Arc<LiftRes>) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(addr, lift_res);
    }

    /// Returns the number of unique machine addresses currently
    /// cached.  Used by [`crate::lifter::Lifter::decode_stats`].
    #[must_use]
    pub fn unique_addresses(&self) -> usize {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.len()
    }

    /// Returns the total number of `get` lookups served by this cache
    /// since construction — both cache hits and cache misses.  Used by
    /// [`crate::lifter::Lifter::decode_stats`].
    #[must_use]
    pub fn total_lift_calls(&self) -> usize {
        self.total_lift_calls.load(Ordering::Relaxed)
    }
}
