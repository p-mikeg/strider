//! Content-keyed cache for [`strider_ir::BuiltFunctionGraph`].
//!
//! Side-table cache keyed by a hash of (binary identity, indirect-targets
//! map).  Stores fully-finalised graphs so a repeat query for the same
//! binary + target set returns the previous result without re-running the
//! orchestrator's fixed-point loop.
//!
//! The cache is the only real mechanism for cross-call reuse: the previous
//! salsa-based orchestrator wrapper added a red/green dependency-tracking
//! layer that never delivered the per-region granularity it was supposed
//! to enable.  Holding finalised BFGs by content hash is what actually
//! gives the repeat-query speed-up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use strider_ir::BuiltFunctionGraph;

/// Mutex-guarded `HashMap<u64, Arc<BuiltFunctionGraph>>`.  Callers hash
/// the inputs they want to key on (binary identity, resolved indirect
/// targets, options) into a single `u64` and use that as the cache key.
#[derive(Default)]
pub struct BfgContentCache {
    inner: Mutex<HashMap<u64, Arc<BuiltFunctionGraph>>>,
}

impl BfgContentCache {
    /// Construct an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a cached graph by content key.  Returns a cloned `Arc` so
    /// the caller can hold the graph beyond the lock scope.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<Arc<BuiltFunctionGraph>> {
        // Mutex poisoning would only happen if a holder panicked mid-write;
        // surfacing the poisoned data here is safe — we never mutate the
        // inner value across operations.
        self.inner.lock().expect("BfgContentCache mutex poisoned").get(&key).cloned()
    }

    /// Insert a graph at `key`.  Overwrites any previous entry; callers
    /// that need first-wins semantics check [`Self::get`] first.
    pub fn insert(&self, key: u64, value: Arc<BuiltFunctionGraph>) {
        self.inner
            .lock()
            .expect("BfgContentCache mutex poisoned")
            .insert(key, value);
    }

    /// Drop every cached entry.
    pub fn clear(&self) {
        self.inner.lock().expect("BfgContentCache mutex poisoned").clear();
    }
}
