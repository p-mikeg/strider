//! Sentinel-fingerprint test wrappers so mock graphs satisfy the
//! always-on Layer-C asm-fingerprint check.  Phase 1 Task 1.4b (G3).
//!
//! Use [`TestGraph`] in tests that build mock graphs by calling
//! [`crate::Graph::create_node`] directly (i.e. bypassing
//! [`crate::FunctionBuilder`]'s `lift_addr` plumbing).  Tests that
//! drive a `FunctionBuilder` can use [`crate::test_utils`]' helpers
//! (`make_empty_fn`, `make_fn_with_var`, `make_sp_fn`) which already
//! set a sentinel `lift_addr` for the duration of the closure.

use crate::graph::Graph;
use crate::node::NodeId;

/// Wraps `Graph` and stamps a unique sentinel asm-fingerprint on every
/// node created through it.
///
/// Production code lifts from real machine instructions and uses
/// [`crate::FunctionBuilder::set_lift_addr`] to stamp real addresses;
/// do NOT use [`TestGraph`] outside `#[cfg(test)]` or `feature =
/// "test-utils"` consumers.
pub struct TestGraph {
    inner: Graph,
    counter: u64,
}

impl TestGraph {
    /// Sentinel base address — distinct from any real machine address
    /// so debugging is unambiguous when a sentinel leaks into
    /// production output.
    pub const SENTINEL_BASE: u64 = 0xDEAD_BEEF_0000_0000;

    /// Builds a fresh empty wrapper.
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Graph::new(), counter: 0 }
    }

    /// Read-only access to the wrapped graph.
    #[must_use]
    pub fn graph(&self) -> &Graph { &self.inner }

    /// Mutable access to the wrapped graph.  Use [`TestGraph::stamp`]
    /// after every `create_node` so the new node carries a sentinel
    /// fingerprint.
    pub fn graph_mut(&mut self) -> &mut Graph { &mut self.inner }

    /// Consumes the wrapper and returns the inner [`Graph`].
    #[must_use]
    pub fn into_inner(self) -> Graph { self.inner }

    /// Stamps a unique sentinel asm-fingerprint on `id`.  Call after
    /// every node creation routed through [`TestGraph::graph_mut`].
    pub fn stamp(&mut self, id: NodeId) {
        self.counter += 1;
        self.inner
            .set_asm_fingerprint(id, vec![Self::SENTINEL_BASE | self.counter]);
    }
}

impl Default for TestGraph {
    fn default() -> Self { Self::new() }
}
