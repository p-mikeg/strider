//! `RegionIrEntry` and `PredecessorHandles`.
//!
//! Holds the IR-side handles a single CFG region exposes to the
//! orchestrator: entry- / exit-control + memory outputs, the entry-
//! boundary `ControlState` / `MemPhi` / `ControlPhi` `NodeId`s (so a
//! later iteration can append a new predecessor input WITHOUT moving
//! existing nodes), and the region's exit `vn_to_value` map for
//! downstream consumers to read.

use std::collections::HashMap;

use cfg::PcodeInsnAddr;
use ir::node::{NodeId, NodeOutputId};
use rsleigh::Vn;

/// IR-side handles for a single CFG region.  See module docs for the
/// invariants each field upholds.
///
/// All `NodeId` / `NodeOutputId` fields are populated by
/// [`crate::cache::lift_new_regions_into`] from the snapshot
/// [`crate::RegionLiftHandles`] returned by the strider lift.  They
/// are valid handles into the corresponding `BuiltFunctionGraph`'s
/// node / output arenas.
#[derive(Debug, Clone)]
pub struct RegionIrEntry {
    /// The `Control` output that flows INTO this region's body
    /// (the `Control` output of the entry `ControlState`).
    pub entry_control: NodeOutputId,
    /// The `Memory` output that flows INTO this region's body
    /// (the `Memory` output of the entry `MemPhi`).
    pub entry_memory: NodeOutputId,
    /// The `Control` output produced at the region's exit
    /// (consumed by successors' `ControlState`).
    pub exit_control: NodeOutputId,
    /// The `Memory` output produced at the region's exit
    /// (consumed by successors' `MemPhi`).
    pub exit_memory: NodeOutputId,
    /// Per-var `ControlPhi` node IDs at the entry boundary.  When a
    /// new predecessor arrives in a later iteration, the orchestrator
    /// adds an input to these existing nodes — it does NOT create
    /// new phi nodes.  This is what keeps the body's IR refs valid
    /// across CFG rebuilds.
    pub entry_var_phis: HashMap<Vn, NodeId>,
    /// The `MemPhi` node ID at the entry boundary.  Same predecessor-
    /// extension role as `entry_var_phis`.
    pub entry_mem_phi: NodeId,
    /// The `ControlState` node ID at the entry boundary.  New
    /// predecessor edges add an input here.
    pub entry_control_state: NodeId,
    /// Per-var values exposed at the region exit, for downstream
    /// regions' phi nodes to read.  Populated from the builder's
    /// region-exit variable map at lift time.
    pub exit_vn_to_value: HashMap<Vn, NodeOutputId>,
    /// Pcode address of the region's start.  Stored so callers (e.g.
    /// `extend_predecessors_into`'s diagnostics) can correlate the
    /// cache entry back to the region without re-querying the CFG.
    pub start_addr: PcodeInsnAddr,
    /// Number of CFG predecessors of this region the cache was
    /// populated against.  Used by `extend_predecessors_into` to
    /// detect "the predecessor count grew since last iteration; phi
    /// inputs must be appended."
    pub cached_predecessor_count: usize,
}

impl RegionIrEntry {
    /// Constructs a sentinel-handle entry for unit tests that don't
    /// need real `NodeId` plumbing.  All `NodeId` / `NodeOutputId`
    /// fields are `from_u32(0)` — must be replaced before use against
    /// a real graph.
    ///
    /// Production callers go through [`Self::from_lift_handles`] instead.
    #[must_use]
    pub fn empty(start_addr: PcodeInsnAddr) -> Self {
        let zero_node = NodeId::from_u32(0);
        let zero_output = NodeOutputId::from_u32(0);
        Self {
            entry_control: zero_output,
            entry_memory: zero_output,
            exit_control: zero_output,
            exit_memory: zero_output,
            entry_var_phis: HashMap::new(),
            entry_mem_phi: zero_node,
            entry_control_state: zero_node,
            exit_vn_to_value: HashMap::new(),
            start_addr,
            cached_predecessor_count: 0,
        }
    }

    /// Populates a [`RegionIrEntry`] from a freshly captured
    /// [`crate::RegionLiftHandles`] snapshot.  This is the production
    /// path used by [`crate::cache::lift_new_regions_into`].
    ///
    /// CORRECTNESS: every field is filled in directly from the
    /// snapshot — no sentinels, no defaults.  Caller is responsible
    /// for ensuring the snapshot was captured against the same
    /// `BuiltFunctionGraph` the cache will be queried against.
    #[must_use]
    pub fn from_lift_handles(handles: &crate::RegionLiftHandles) -> Self {
        Self {
            entry_control: handles.entry_control,
            entry_memory: handles.entry_memory,
            exit_control: handles.exit_control,
            exit_memory: handles.exit_memory,
            entry_var_phis: handles.entry_var_phis.clone(),
            entry_mem_phi: handles.entry_mem_phi,
            entry_control_state: handles.entry_control_state,
            exit_vn_to_value: handles.exit_vn_to_value.clone(),
            start_addr: handles.start_addr,
            cached_predecessor_count: handles.predecessor_count,
        }
    }
}

/// IR-side handles for a region's *exit* boundary, packaged as a
/// "predecessor handle" the
/// [`crate::cache::extend_predecessors_with_handle`] helpers consume.
///
/// Used to wire a NEW predecessor edge into an existing region's phi
/// nodes: the predecessor's exit control + memory + per-var values are
/// what the cached region's `ControlState` / `MemPhi` /
/// `entry_var_phis` need as new inputs.
#[derive(Debug, Clone)]
pub struct PredecessorHandles {
    /// Exit control output of the predecessor.
    pub exit_control: NodeOutputId,
    /// Exit memory output of the predecessor.
    pub exit_memory: NodeOutputId,
    /// Per-var exit values of the predecessor, keyed by `Vn`.
    /// Variables not in this map fall back to `InitialVar(vn)` —
    /// mirrors the IR builder's convention for vars that aren't
    /// live across the edge.
    pub exit_vn_to_value: HashMap<Vn, NodeOutputId>,
}
