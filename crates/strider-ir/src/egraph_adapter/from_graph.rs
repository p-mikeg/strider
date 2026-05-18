//! `EGraphAdapter::from_graph` — converts a strider [`crate::Graph`] into an
//! `egg::EGraph<StriderLang, ()>` walking only the value subgraph.
//!
//! Phase 1 Task 1.5 spike. Stub — populated in step 3 of the task.

use std::collections::HashMap;

use egg::{EGraph, Id};

use super::language::StriderLang;
use crate::node::NodeId;

/// Adapter holding the egraph plus the two NodeId↔egg-Id mapping tables
/// needed to round-trip a strider graph through egg with zero rewrites.
pub struct EGraphAdapter {
    /// The egg egraph built from the value-slice subgraph of the source
    /// [`crate::Graph`].
    pub egraph: EGraph<StriderLang, ()>,
    /// Maps every strider `NodeId` added to the egraph to its e-class id.
    pub node_to_eclass: HashMap<NodeId, Id>,
    /// Reverse map for opaque leaves: opaque-payload `u64` → strider `NodeId`.
    /// Used by `extract_into_graph` to recover the original node identity
    /// when an opaque-leaf e-class is extracted.
    pub leaf_to_node: HashMap<u64, NodeId>,
}
