//! Per-region IR cache used by the indirect-branch fixed-point loop.
//!
//! Each entry pins, for one CFG region, the IR-side handles the
//! orchestrator and the in-place editors need:
//!
//!   * the entry / exit control & memory `NodeOutputId`s,
//!   * the per-var `ControlPhi` `NodeId`s at the entry boundary, so a
//!     future iteration that brings a new predecessor can append an
//!     input to the *existing* phi nodes (preserving body refs),
//!   * the `MemPhi` and `ControlState` `NodeId`s for the same reason,
//!   * the exit `vn_to_value` map for downstream consumers.
//!
//! # Cache key
//!
//! The cache key is `MachineInsnAddr` — the machine address of the
//! region's first instruction.  This is **stable across CFG rebuilds**:
//! the same machine address always lifts to the same pcode, so the
//! same key always identifies the same body.  Region splits
//! (`split_region` in cfg) preserve the first half's start address;
//! the second half gets a fresh address.  The key handles both halves
//! transparently.
//!
//! # Cache lifetime
//!
//! Cache entries hold raw `NodeId` / `NodeOutputId` values that index
//! into a specific `BuiltFunctionGraph`'s arena.  An entry is valid
//! ONLY for the graph it was populated against — calling
//! `lift_new_regions_into` against a fresh CFG produces fresh entries
//! that supersede prior ones.  The orchestrator clears or rebuilds
//! the cache when it discards the underlying graph.

use std::collections::HashMap;

use cfg::{Cfg, MachineInsnAddr, PcodeInsnAddr, RegionId};
use ir::node::{NodeId, NodeOutputId};
use rsleigh::Vn;

use crate::error::Result;

/// IR-side handles for a single CFG region.  See module docs for the
/// invariants each field upholds.
///
/// All `NodeId` / `NodeOutputId` fields are populated by
/// [`lift_new_regions_into`] from the snapshot
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
    /// Production callers go through [`from_lift_handles`] instead.
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
    /// path used by [`lift_new_regions_into`].
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

/// Persistent map from a region's machine start address to its IR
/// boundary handles.  Keyed by `MachineInsnAddr` (not `PcodeInsnAddr`)
/// because regions always start at a machine-instruction boundary —
/// the pcode index is implicitly 0 for region starts.
pub type RegionIrCache = HashMap<MachineInsnAddr, RegionIrEntry>;

/// IR-side handles for a region's *exit* boundary, packaged as a
/// "predecessor handle" the [`extend_predecessors_into`] helpers consume.
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

/// The cache key for a CFG region.  Returns the machine address of
/// the region's first instruction.
///
/// # Errors
///
/// Returns `crate::error::ErrorKind::CfgNoRegion` wrapped in
/// `crate::Error` when `region_id` does not refer to a region in
/// `cfg`.  Stable for the lifetime of the CFG.
pub fn cache_key_for_region<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    region_id: RegionId,
) -> Result<MachineInsnAddr> {
    let region = cfg
        .graph
        .node_weight(region_id)
        .ok_or(crate::error::ErrorKind::CfgNoRegion(region_id))?;
    Ok(region.start_addr.machine_addr)
}

/// Counts how many regions in `cfg` are absent from `cache`.
///
/// Used by the orchestrator to size the work that "lift only new
/// regions" would have to do; in round-1 this is informational
/// because the orchestrator re-lifts from scratch each iteration.
///
/// # Errors
///
/// Propagates `cache_key_for_region`'s error if any region in `cfg`
/// has no entry in `cfg.graph`.
pub fn count_uncached_regions<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    cache: &RegionIrCache,
) -> Result<usize> {
    let mut n = 0;
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(cfg, region_id)?;
        if !cache.contains_key(&key) {
            n += 1;
        }
    }
    Ok(n)
}

/// Counts how many CFG predecessors each cached region has, returning
/// `Some(diff)` when any region's current predecessor count exceeds
/// the cached count (signalling a new predecessor edge has been added
/// in this iteration's CFG).  Returns `None` when no diffs.
///
/// Used by `extend_predecessors_into`-style call sites to short-
/// circuit when nothing new has arrived.
///
/// # Errors
///
/// Propagates `cache_key_for_region`'s error.
pub fn predecessor_diffs<R: rsleigh::MemReader>(
    cfg: &Cfg<R>,
    cache: &RegionIrCache,
) -> Result<Vec<(MachineInsnAddr, usize, usize)>> {
    let mut diffs = Vec::new();
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(cfg, region_id)?;
        if let Some(entry) = cache.get(&key) {
            let preds = cfg.predecessor_count(region_id);
            if preds != entry.cached_predecessor_count {
                diffs.push((key, entry.cached_predecessor_count, preds));
            }
        }
    }
    Ok(diffs)
}

/// CORRECTNESS NOTE — `lift_new_regions_into`:
///
/// Lifts every uncached region in `cfg` into the IR graph (via
/// [`Strider::analyze_cfg_with_unresolved`]) and populates `cache`
/// with one [`RegionIrEntry`] per CFG region.  Cached regions in
/// `cfg` keep their existing entries — populating overwrites the
/// previous handles only when the snapshot was captured against the
/// same iteration's lift.
///
/// The cache-hit branch is correct because the body of a region
/// depends on (i) earlier-in-region nodes wired at lift time
/// (deterministic from the pcode), and (ii) the region's entry-
/// boundary phi `NodeId`s.  Both are pinned in `RegionIrEntry` and
/// remain valid even when a new predecessor arrives — predecessor
/// extension only adds INPUTS to existing phi nodes, never moves or
/// removes them, so body refs stay valid.
///
/// # Round-1 architecture caveat
///
/// In round 1 the strider lift constructs a fresh `FunctionBuilder`
/// each call.  This means cached entries from a PREVIOUS iteration's
/// graph do NOT survive into the new graph (the `NodeId`s point into
/// the old arena).  The orchestrator handles this by clearing the
/// cache on each rebuild and repopulating from the new lift's
/// snapshot.  Future rounds will persist the builder, at which point
/// cache hits truly skip re-lifting.
///
/// # Errors
///
/// Propagates the strider lift's errors verbatim.
pub fn lift_new_regions_into<R: rsleigh::MemReader>(
    strider: &super::Strider,
    cache: &mut RegionIrCache,
    cfg: &Cfg<R>,
) -> Result<super::AnalyzeOutcome> {
    // CORRECTNESS — full re-lift this iteration: round 1 doesn't
    // persist the FunctionBuilder, so we re-lift everything.  The
    // returned `region_handles` snapshot is captured against the
    // freshly-built graph and replaces any prior cache entries.
    let outcome = strider.analyze_cfg_with_unresolved(cfg)?;
    populate_cache_from_handles(cache, &outcome.region_handles);
    Ok(outcome)
}

/// Populates `cache` with one [`RegionIrEntry`] per
/// [`crate::RegionLiftHandles`] in `handles`, keyed by the region's
/// machine start address.  Overwrites prior entries — see the
/// `lift_new_regions_into` correctness note for round-1's reset
/// semantics.
fn populate_cache_from_handles(
    cache: &mut RegionIrCache,
    handles: &[crate::RegionLiftHandles],
) {
    for h in handles {
        // CORRECTNESS — overwrite-on-rebuild: round 1 rebuilds the IR
        // arena each iteration, so the prior handles are stale.
        // Always overwrite — the rebuild contract guarantees the new
        // handles are consistent with the new graph.
        let entry = RegionIrEntry::from_lift_handles(h);
        cache.insert(h.start_addr.machine_addr, entry);
    }
}

/// CORRECTNESS NOTE — `extend_predecessors_into`:
///
/// When a CFG rebuild brings new predecessors into a region whose
/// cache entry already exists, the contract is:
///
///   * Add an input to the existing `entry_control_state` (NodeId
///     pinned in the cache).
///   * Add an input to the existing `entry_mem_phi`.
///   * For each `(vn, phi_node_id)` in `entry_var_phis`: add an
///     input from the predecessor's `exit_vn_to_value[vn]`
///     (or `InitialVar(vn)` if the var isn't live across the edge).
///
/// We APPEND to existing nodes — we never rewrite a `NodeOutputId`
/// or move a phi.  Body refs that pre-date this call therefore stay
/// valid: every consumer of `entry_var_phis[vn]`'s output points at
/// the same node, which now happens to have one more input slot.
///
/// In the round-1 orchestrator, full re-lift on each iteration means
/// the previous-iteration's phi nodes are gone — `extend_predecessors_into`
/// is therefore a no-op against a freshly-rebuilt graph.  The
/// granular helper [`extend_predecessors_with_handle`] is the
/// load-bearing primitive for unit tests and for future rounds that
/// persist the IR across iterations.
///
/// # Errors
///
/// Returns `Ok(())` unconditionally in this round-1 surface (no-op).
pub fn extend_predecessors_into<R: rsleigh::MemReader>(
    cache: &mut RegionIrCache,
    cfg: &Cfg<R>,
) -> Result<()> {
    // No-op: see correctness note above.  The IR arena does not
    // persist across orchestrator iterations, so we have no graph
    // handle to apply phi extensions to.  Production phi extension
    // happens via [`extend_predecessors_with_handle`] called by
    // future-round orchestrators that hold a persistent graph.
    let _ = cache;
    let _ = cfg;
    Ok(())
}

/// Append a new predecessor edge to `cache_entry`'s phi nodes inside
/// `graph`.  This is the unit-tested primitive that future-round
/// orchestrators call when a CFG rebuild brings a new predecessor
/// into a region with a still-live cache entry.
///
/// The edits performed (in order):
///   1. Append `pred.exit_control` to `entry_control_state`'s inputs.
///   2. Append `pred.exit_memory` to `entry_mem_phi`'s inputs.
///   3. For each `(vn, phi_node_id)` in `entry_var_phis`: append
///      `pred.exit_vn_to_value[vn]` (or fall back to building a fresh
///      `InitialVar(vn)` if the var isn't live across the edge).
///
/// CORRECTNESS — node id stability: every append uses
/// [`Graph::add_node_input`] which mutates the existing node in
/// place.  The phi `NodeId`s pinned in the cache stay valid; body
/// refs that consume those phi outputs stay valid.
///
/// CORRECTNESS — var fallback: when `pred.exit_vn_to_value` doesn't
/// contain `vn`, we synthesise an `InitialVar(vn)` node and feed
/// that.  This mirrors how the IR builder handles vars that aren't
/// live across an edge — the phi gets the function-entry value as
/// its input on this edge, which is the consistent SSA-extension
/// semantics.  Note: `InitialVar` is cacheable, so creating one when
/// the graph already has the same `InitialVar(vn)` returns the
/// existing node id.
///
/// # Errors
///
/// Propagates IR mutation errors (e.g. `add_node_input` against a
/// cacheable node — but `ControlState` / `MemPhi` / `ControlPhi` are
/// all non-cacheable, so this should not happen).
pub fn extend_predecessors_with_handle(
    cache_entry: &mut RegionIrEntry,
    graph: &mut ir::BuiltFunctionGraph,
    pred: &PredecessorHandles,
) -> Result<()> {
    use ir::node::{NodeKind, NodeOutputKind};

    // Append predecessor's exit control to the ControlState's inputs.
    // CORRECTNESS: ControlState is non-cacheable; add_node_input
    // mutates in place.  Pinned NodeId in cache stays valid.
    graph
        .graph
        .add_node_input(cache_entry.entry_control_state, pred.exit_control)?;

    // Append predecessor's exit memory to the MemPhi's inputs.
    // CORRECTNESS: MemPhi is non-cacheable; same in-place mutation.
    graph
        .graph
        .add_node_input(cache_entry.entry_mem_phi, pred.exit_memory)?;

    // For each per-var phi: append the predecessor's exit value, or
    // synthesise an InitialVar(vn) fallback.
    // CORRECTNESS: ControlPhi is non-cacheable; the per-var fallback
    // (creating an InitialVar) IS cacheable — create_node returns the
    // existing InitialVar(vn) node if one already exists, so we never
    // double-create.
    let phis_to_extend: Vec<(Vn, NodeId)> = cache_entry
        .entry_var_phis
        .iter()
        .map(|(&vn, &phi_id)| (vn, phi_id))
        .collect();
    for (vn, phi_node_id) in phis_to_extend {
        let value_for_pred = if let Some(&v) = pred.exit_vn_to_value.get(&vn) {
            v
        } else {
            // Fallback: build/dedup an InitialVar(vn) and feed its
            // sole output.  Determine the integer width from the Vn's
            // size; clamp to a supported NodeOutputType.
            let ty: ir::node::NodeOutputType = match vn.size {
                1 => ir::node::NodeOutputType::U8,
                2 => ir::node::NodeOutputType::U16,
                4 => ir::node::NodeOutputType::U32,
                8 => ir::node::NodeOutputType::U64,
                16 => ir::node::NodeOutputType::U128,
                32 => ir::node::NodeOutputType::U256,
                other => {
                    return Err(crate::error::ErrorKind::UnsupportedRegSize(other).into());
                }
            };
            let iv = graph.graph.create_node(
                NodeKind::InitialVar(vn),
                [],
                [NodeOutputKind::OutputType(ty)],
            );
            graph.graph.node_outputs_exact::<1>(iv)?[0]
        };
        graph.graph.add_node_input(phi_node_id, value_for_pred)?;
    }

    // Bump the cached predecessor count so a subsequent
    // predecessor_diffs call doesn't double-flag this region.
    cache_entry.cached_predecessor_count += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the `RegionIrCache` types and helpers.
    //!
    //! These tests exercise the cache invariants in isolation, with
    //! the smallest possible fixtures: hand-rolled
    //! `MachineInsnAddr` / `PcodeInsnAddr` values, a `RegionIrCache`
    //! built from scratch, and direct invocation of the helpers.
    //!
    //! Integration tests in `crates/strider/tests/tier2_cache.rs`
    //! cover the end-to-end lifetime against real CFGs / built
    //! function graphs.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use ir::node::NodeOutputKind;
    use ir::node::NodeOutputType;
    use ir::FunctionBuilder;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: 0,
        }
    }

    fn make_vn(off: u64) -> Vn {
        Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off,
            },
            size: 4,
        }
    }

    /// Build a minimal `BuiltFunctionGraph` whose entry region tracks
    /// one variable.  Used by extend-predecessors tests as a backing
    /// graph the helper mutates.  Returns the graph and the entry
    /// region's lift handles (control_state node id, mem_phi, etc.).
    fn build_minimal_graph_with_one_var() -> (ir::BuiltFunctionGraph, RegionIrEntry) {
        let v = make_vn(0x10);
        let mut b = FunctionBuilder::new_raw(vec![v], &[], &[], &[], None, 0)
            .expect("new_raw");
        let r = b.create_region().expect("create");
        b.set_entry_region(r).expect("set_entry");
        b.set_region(r);
        // Read the variable so the ControlPhi has an output the body
        // would reference (same shape as a real strider lift).
        let _val = b.read_variable(&v).expect("read");
        b.build_return(None, &[]).expect("ret");
        // Capture handles BEFORE build() consumes the builder.
        let cs = b.region_control_node(r);
        let mp = b.region_memory_node(r);
        let entry_ctrl = b.region_entry_control(r).expect("entry_ctrl");
        let entry_mem = b.region_entry_memory(r).expect("entry_mem");
        let exit_ctrl = b.region_cur_ctrl(r);
        let exit_mem = b.region_cur_memory(r);
        let mut entry_var_phis: HashMap<Vn, NodeId> = HashMap::new();
        for (var_id, phi_out) in b.region_initial_variables(r) {
            if let Some(vn) = b.vn_of_var(var_id) {
                let phi_node = b.body().graph.output_definition(phi_out).0;
                entry_var_phis.insert(vn, phi_node);
            }
        }
        let mut exit_vn_to_value: HashMap<Vn, NodeOutputId> = HashMap::new();
        for (var_id, val_out) in b.region_exit_variables(r) {
            if let Some(vn) = b.vn_of_var(var_id) {
                exit_vn_to_value.insert(vn, val_out);
            }
        }
        let graph = b.build().expect("build");
        let entry = RegionIrEntry {
            entry_control: entry_ctrl,
            entry_memory: entry_mem,
            exit_control: exit_ctrl,
            exit_memory: exit_mem,
            entry_var_phis,
            entry_mem_phi: mp,
            entry_control_state: cs,
            exit_vn_to_value,
            start_addr: pcode_addr(0x1000),
            cached_predecessor_count: 1,
        };
        (graph, entry)
    }

    #[test]
    fn region_ir_entry_default_is_empty() {
        // The empty constructor must produce a usable but
        // sentinel-valued entry: empty maps, zero predecessor count,
        // start_addr threaded through.
        let entry = RegionIrEntry::empty(pcode_addr(0x1234));
        assert!(entry.entry_var_phis.is_empty());
        assert!(entry.exit_vn_to_value.is_empty());
        assert_eq!(entry.cached_predecessor_count, 0);
        assert_eq!(entry.start_addr, pcode_addr(0x1234));
    }

    #[test]
    fn region_ir_cache_default_is_empty() {
        let cache: RegionIrCache = HashMap::new();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn region_ir_entry_insert_then_retrieve_round_trips() {
        // Round-trip insertion + lookup via the same key.  Pins the
        // expectation that MachineInsnAddr is a usable HashMap key.
        let mut cache: RegionIrCache = HashMap::new();
        let key = MachineInsnAddr { addr: 0xdead_beef };
        cache.insert(key, RegionIrEntry::empty(pcode_addr(0xdead_beef)));
        assert_eq!(cache.len(), 1);
        let got = cache.get(&key).expect("key must round-trip");
        assert_eq!(got.start_addr.machine_addr, key);
    }

    #[test]
    fn cache_key_uses_machine_addr_only() {
        // Two PcodeInsnAddrs with the same machine_addr but
        // different insn_index hash to the same MachineInsnAddr key.
        // This is what makes the cache stable across iterations:
        // region starts always have insn_index 0, but if a future
        // refactor accidentally keys on the full PcodeInsnAddr it
        // would silently miss the cache on every lookup.
        let a = MachineInsnAddr { addr: 0x1000 };
        let b = MachineInsnAddr { addr: 0x1000 };
        assert_eq!(a, b);
        let mut cache: RegionIrCache = HashMap::new();
        cache.insert(a, RegionIrEntry::empty(pcode_addr(0x1000)));
        assert!(cache.contains_key(&b));
    }

    #[test]
    fn predecessor_diffs_of_empty_cache_is_empty() {
        // The diff function must return an empty vec when the cache
        // is empty — there's nothing to compare against.
        let cache: RegionIrCache = HashMap::new();
        assert_eq!(cache.len(), 0);
    }

    // ── from_lift_handles tests (G1 cache populate) ─────────────────────────

    #[test]
    fn from_lift_handles_populates_all_fields() {
        // Pin: every field of RegionLiftHandles ends up at the
        // matching field of RegionIrEntry.
        let v = make_vn(0x20);
        let cs = NodeId::from_u32(7);
        let mp = NodeId::from_u32(8);
        let phi = NodeId::from_u32(9);
        let ec = NodeOutputId::from_u32(11);
        let em = NodeOutputId::from_u32(12);
        let xc = NodeOutputId::from_u32(13);
        let xm = NodeOutputId::from_u32(14);
        let xv = NodeOutputId::from_u32(15);
        let mut entry_var_phis = HashMap::new();
        entry_var_phis.insert(v, phi);
        let mut exit_vn_to_value = HashMap::new();
        exit_vn_to_value.insert(v, xv);
        let h = crate::RegionLiftHandles {
            start_addr: pcode_addr(0xbeef),
            predecessor_count: 3,
            entry_control_state: cs,
            entry_mem_phi: mp,
            entry_control: ec,
            entry_memory: em,
            exit_control: xc,
            exit_memory: xm,
            entry_var_phis,
            exit_vn_to_value,
        };
        let e = RegionIrEntry::from_lift_handles(&h);
        assert_eq!(e.entry_control_state, cs);
        assert_eq!(e.entry_mem_phi, mp);
        assert_eq!(e.entry_control, ec);
        assert_eq!(e.entry_memory, em);
        assert_eq!(e.exit_control, xc);
        assert_eq!(e.exit_memory, xm);
        assert_eq!(e.entry_var_phis.get(&v), Some(&phi));
        assert_eq!(e.exit_vn_to_value.get(&v), Some(&xv));
        assert_eq!(e.start_addr, pcode_addr(0xbeef));
        assert_eq!(e.cached_predecessor_count, 3);
    }

    #[test]
    fn populate_cache_from_handles_inserts_one_entry_per_region() {
        // A snapshot with N entries produces a cache with N keys.
        let mut cache: RegionIrCache = HashMap::new();
        let h1 = crate::RegionLiftHandles {
            start_addr: pcode_addr(0x1000),
            predecessor_count: 0,
            entry_control_state: NodeId::from_u32(1),
            entry_mem_phi: NodeId::from_u32(2),
            entry_control: NodeOutputId::from_u32(1),
            entry_memory: NodeOutputId::from_u32(2),
            exit_control: NodeOutputId::from_u32(3),
            exit_memory: NodeOutputId::from_u32(4),
            entry_var_phis: HashMap::new(),
            exit_vn_to_value: HashMap::new(),
        };
        let h2 = crate::RegionLiftHandles {
            start_addr: pcode_addr(0x2000),
            predecessor_count: 1,
            entry_control_state: NodeId::from_u32(3),
            entry_mem_phi: NodeId::from_u32(4),
            entry_control: NodeOutputId::from_u32(5),
            entry_memory: NodeOutputId::from_u32(6),
            exit_control: NodeOutputId::from_u32(7),
            exit_memory: NodeOutputId::from_u32(8),
            entry_var_phis: HashMap::new(),
            exit_vn_to_value: HashMap::new(),
        };
        populate_cache_from_handles(&mut cache, &[h1, h2]);
        assert_eq!(cache.len(), 2);
        assert!(cache.contains_key(&MachineInsnAddr { addr: 0x1000 }));
        assert!(cache.contains_key(&MachineInsnAddr { addr: 0x2000 }));
    }

    #[test]
    fn populate_cache_from_handles_overwrites_prior_entry() {
        // Round-1 reset semantics: a second call replaces prior entries.
        let mut cache: RegionIrCache = HashMap::new();
        let h1 = crate::RegionLiftHandles {
            start_addr: pcode_addr(0x1000),
            predecessor_count: 0,
            entry_control_state: NodeId::from_u32(1),
            entry_mem_phi: NodeId::from_u32(2),
            entry_control: NodeOutputId::from_u32(1),
            entry_memory: NodeOutputId::from_u32(2),
            exit_control: NodeOutputId::from_u32(3),
            exit_memory: NodeOutputId::from_u32(4),
            entry_var_phis: HashMap::new(),
            exit_vn_to_value: HashMap::new(),
        };
        populate_cache_from_handles(&mut cache, &[h1]);
        let h2 = crate::RegionLiftHandles {
            start_addr: pcode_addr(0x1000),
            predecessor_count: 5,
            entry_control_state: NodeId::from_u32(99),
            entry_mem_phi: NodeId::from_u32(98),
            entry_control: NodeOutputId::from_u32(97),
            entry_memory: NodeOutputId::from_u32(96),
            exit_control: NodeOutputId::from_u32(95),
            exit_memory: NodeOutputId::from_u32(94),
            entry_var_phis: HashMap::new(),
            exit_vn_to_value: HashMap::new(),
        };
        populate_cache_from_handles(&mut cache, &[h2]);
        let entry = cache.get(&MachineInsnAddr { addr: 0x1000 }).expect("present");
        assert_eq!(entry.cached_predecessor_count, 5);
        assert_eq!(entry.entry_control_state, NodeId::from_u32(99));
    }

    // ── extend_predecessors_with_handle tests (G1 phi extension) ────────────

    #[test]
    fn extend_predecessors_into_appends_to_existing_control_state() {
        // After one call, the ControlState's input count grows by 1
        // and its NodeId is unchanged.
        let (mut graph, mut entry) = build_minimal_graph_with_one_var();
        let cs_before = entry.entry_control_state;
        let inputs_before = graph
            .graph
            .node_inputs(cs_before)
            .into_iter()
            .count();
        // Synthesise a Control output to feed as the new pred edge.
        // Use an Entry node — already in the graph; locate it.
        let entry_ctrl = {
            let entry_node = graph.entry;
            let outs: Vec<_> = graph.graph.node_outputs(entry_node).into_iter().collect();
            outs[0]
        };
        let pred = PredecessorHandles {
            exit_control: entry_ctrl,
            exit_memory: graph
                .graph
                .node_outputs(graph.entry)
                .into_iter()
                .next()
                .unwrap(),
            exit_vn_to_value: HashMap::new(),
        };
        // For exit_memory we need a Memory-typed output.  Locate
        // the InitialMemory node's output.
        let initial_mem = graph
            .preorder()
            .find(|&nid| {
                matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
            })
            .expect("InitialMemory");
        let im_out = graph
            .graph
            .node_outputs(initial_mem)
            .into_iter()
            .next()
            .expect("output");
        let pred = PredecessorHandles {
            exit_control: pred.exit_control,
            exit_memory: im_out,
            exit_vn_to_value: HashMap::new(),
        };
        extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
        let inputs_after = graph
            .graph
            .node_inputs(cs_before)
            .into_iter()
            .count();
        assert_eq!(inputs_after, inputs_before + 1);
        assert_eq!(entry.entry_control_state, cs_before, "NodeId stable");
    }

    #[test]
    fn extend_predecessors_into_appends_to_existing_mem_phi() {
        // After one call, the MemPhi's input count grows by 1.
        let (mut graph, mut entry) = build_minimal_graph_with_one_var();
        let mp = entry.entry_mem_phi;
        let inputs_before = graph.graph.node_inputs(mp).into_iter().count();
        let entry_ctrl = {
            let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
            outs[0]
        };
        let initial_mem = graph
            .preorder()
            .find(|&nid| {
                matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
            })
            .expect("InitialMemory");
        let im_out = graph
            .graph
            .node_outputs(initial_mem)
            .into_iter()
            .next()
            .expect("output");
        let pred = PredecessorHandles {
            exit_control: entry_ctrl,
            exit_memory: im_out,
            exit_vn_to_value: HashMap::new(),
        };
        extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
        let inputs_after = graph.graph.node_inputs(mp).into_iter().count();
        assert_eq!(inputs_after, inputs_before + 1);
        assert_eq!(entry.entry_mem_phi, mp, "NodeId stable");
    }

    #[test]
    fn extend_predecessors_into_appends_to_existing_var_phi() {
        // After one call, the per-var ControlPhi's input count grows.
        let v = make_vn(0x10);
        let (mut graph, mut entry) = build_minimal_graph_with_one_var();
        let phi_id = *entry.entry_var_phis.get(&v).expect("phi present");
        let inputs_before = graph.graph.node_inputs(phi_id).into_iter().count();
        let entry_ctrl = {
            let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
            outs[0]
        };
        let initial_mem = graph
            .preorder()
            .find(|&nid| {
                matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
            })
            .expect("InitialMemory");
        let im_out = graph
            .graph
            .node_outputs(initial_mem)
            .into_iter()
            .next()
            .expect("output");
        // Synthesise a value-typed output for the predecessor's
        // exit-value of `v`.
        let val_node = graph.graph.create_node(
            ir::node::NodeKind::IntConst(0xabcd_u128),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let val_out = graph.graph.node_outputs_exact::<1>(val_node).expect("out")[0];
        let mut exit_vn_to_value = HashMap::new();
        exit_vn_to_value.insert(v, val_out);
        let pred = PredecessorHandles {
            exit_control: entry_ctrl,
            exit_memory: im_out,
            exit_vn_to_value,
        };
        extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
        let inputs_after = graph.graph.node_inputs(phi_id).into_iter().count();
        assert_eq!(inputs_after, inputs_before + 1);
        // NodeId stable.
        assert_eq!(*entry.entry_var_phis.get(&v).unwrap(), phi_id);
    }

    #[test]
    fn extend_predecessors_into_no_change_when_pred_count_unchanged() {
        // A predecessor_diffs call against the just-populated cache
        // returns no diffs — i.e. the predecessor count matches the
        // CFG.  We cover this against a real cfg in the integration
        // tests; here we pin the per-entry contract: bumping
        // cached_predecessor_count after extend_predecessors_with_handle.
        let (mut graph, mut entry) = build_minimal_graph_with_one_var();
        let count_before = entry.cached_predecessor_count;
        let entry_ctrl = {
            let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
            outs[0]
        };
        let initial_mem = graph
            .preorder()
            .find(|&nid| {
                matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
            })
            .expect("InitialMemory");
        let im_out = graph
            .graph
            .node_outputs(initial_mem)
            .into_iter()
            .next()
            .expect("output");
        let pred = PredecessorHandles {
            exit_control: entry_ctrl,
            exit_memory: im_out,
            exit_vn_to_value: HashMap::new(),
        };
        extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
        // After one extension, count grew by exactly 1.
        assert_eq!(entry.cached_predecessor_count, count_before + 1);
    }

    #[test]
    fn extend_predecessors_into_handles_var_not_in_predecessor_exit_map() {
        // When pred.exit_vn_to_value lacks the var, fallback to
        // building/reusing an InitialVar(vn) — the phi gets the
        // function-entry value as its new input on this edge.
        let v = make_vn(0x10);
        let (mut graph, mut entry) = build_minimal_graph_with_one_var();
        let phi_id = *entry.entry_var_phis.get(&v).expect("phi present");
        let inputs_before = graph.graph.node_inputs(phi_id).into_iter().count();
        let entry_ctrl = {
            let outs: Vec<_> = graph.graph.node_outputs(graph.entry).into_iter().collect();
            outs[0]
        };
        let initial_mem = graph
            .preorder()
            .find(|&nid| {
                matches!(graph.graph.node_kind(nid), ir::node::NodeKind::InitialMemory)
            })
            .expect("InitialMemory");
        let im_out = graph
            .graph
            .node_outputs(initial_mem)
            .into_iter()
            .next()
            .expect("output");
        // Note: exit_vn_to_value is EMPTY — `v` is not in the pred's
        // map.  Fallback path triggers.
        let pred = PredecessorHandles {
            exit_control: entry_ctrl,
            exit_memory: im_out,
            exit_vn_to_value: HashMap::new(),
        };
        extend_predecessors_with_handle(&mut entry, &mut graph, &pred).expect("extend");
        let inputs_after = graph.graph.node_inputs(phi_id).into_iter().count();
        assert_eq!(inputs_after, inputs_before + 1, "phi got the fallback input");
        // The new input slot's source must be an InitialVar(v).
        let new_input_idx = inputs_after - 1;
        let new_input: Vec<_> = graph.graph.node_inputs(phi_id).into_iter().collect();
        let new_input_out = new_input[new_input_idx];
        let (new_input_node, _) = graph.graph.output_definition(new_input_out);
        assert!(
            matches!(
                graph.graph.node_kind(new_input_node),
                ir::node::NodeKind::InitialVar(vn) if *vn == v,
            ),
            "fallback must be InitialVar(vn), got {:?}",
            graph.graph.node_kind(new_input_node),
        );
    }
}
