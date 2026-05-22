//! Sleigh-decoded `LiftRes` cache shared across CFG rebuilds.
//!
//! During the strider orchestrator's indirect-branch fixed-point, the
//! `Cfg` is rebuilt every iteration that resolves a new target.  Each
//! rebuild lifts every byte through Sleigh, paying for every machine
//! instruction's decode again — even though the bytes are identical
//! and the Sleigh context is the same.
//!
//! This cache lets callers persist `(machine_addr) → Rc<LiftRes>`
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
//!    cached `Rc` can be cheaply cloned by every consumer of an
//!    address.
//! 3. **Single-threaded.**  The orchestrator runs sequentially within
//!    one `strider::run` invocation, so the cache uses `Rc<RefCell>`
//!    rather than `Arc<Mutex>` — no atomic ops, no poison recovery.
//!
//! The orchestrator constructs one `DecodeCache` at the top of
//! `strider::run`, threads it into every `cfg::Builder` via
//! [`crate::cfg::Builder::with_decode_cache`], and discards it when the
//! `run` returns.

use std::cell::RefCell;
use std::rc::Rc;

use rsleigh::LiftRes;
use rustc_hash::FxHashMap;

/// Shared-ownership cache from machine address to Sleigh's lifted
/// pcode.  Cheap to clone (single `Rc::clone`).
#[derive(Clone, Default)]
pub struct DecodeCache {
    inner: Rc<RefCell<FxHashMap<u64, Rc<LiftRes>>>>,
}

impl DecodeCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(FxHashMap::default())),
        }
    }

    /// Returns the cached `LiftRes` for `addr` if one has been
    /// recorded, else `None`.
    #[must_use]
    pub fn get(&self, addr: u64) -> Option<Rc<LiftRes>> {
        self.inner.borrow().get(&addr).cloned()
    }

    /// Records `lift_res` for `addr`.  Replaces any prior entry.
    pub fn insert(&self, addr: u64, lift_res: Rc<LiftRes>) {
        self.inner.borrow_mut().insert(addr, lift_res);
    }
}
