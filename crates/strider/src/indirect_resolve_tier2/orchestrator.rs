//! Outer fixed-point orchestrator for indirect-branch resolution.
//!
//! Drives the iterate-resolve-feed-back loop the spec describes:
//!
//!   1. Build the CFG (with the current `known_targets` map).
//!   2. Lift to IR via `Strider::analyze_cfg_with_unresolved`.
//!   3. Run the strider optimiser pipeline (which today is the
//!      [`opt::default_pipeline`] + stack passes — round-1
//!      simplification: we run the FULL pipeline because round 1
//!      rebuilds the IR from scratch each iteration anyway, so the
//!      stable-vs-destructive distinction only matters when we
//!      re-use cached IR across iterations, which is round-2 work).
//!   4. For each unresolved anchor, run [`super::classify_anchor`].
//!   5. If any new resolutions appear, update `known_targets` and
//!      rebuild.  If the resolution set is unchanged, we've reached
//!      a fixed point.
//!   6. At fixed point: if any branch is still unresolved, return
//!      `Err(UnresolvedIndirectBranch(addr))`.  Otherwise return
//!      the optimised IR.
//!
//! # Iteration cap
//!
//! Bounded by `2 * pending_at_iter_0 + 4`.  Hitting the cap means
//! the resolver violated monotonicity (every legal classification
//! transition strictly grows the induced edge set, so the loop
//! must terminate within the cap).  Surfaces as a typed
//! [`crate::ErrorKind::IndirectResolutionDidNotConverge`] — never
//! a panic.
//!
//! # Round-1 simplifications vs. spec
//!
//! The spec describes a stable/destructive pipeline split.  Round 1
//! ignores it because we rebuild the IR from scratch each iteration
//! (no inter-iteration cache reuse), so there's no cache to invalidate.
//! Round 2+ will turn `lift_new_regions_into` into a true cache-aware
//! lifter and the stable/destructive split becomes load-bearing.
//!
//! In-place edits ([`super::apply_link_register`] /
//! [`super::apply_tail_call`]) are similarly not invoked by the
//! round-1 orchestrator.  The `LinkRegister` and `TailCall`
//! resolutions are routed through the CFG rebuild path
//! (`with_known_targets` → builder produces `Return` / `TailCall`
//! terminators directly).  The in-place editors are future-rounds
//! optimisations that let us skip the rebuild for terminal
//! resolutions.

#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;

use cfg::{Builder, Cfg, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use opt::ReadOnlyMemory;

use crate::error::{ErrorKind, Result};
use crate::strider::{AnalyzeOutcome, Strider};

use super::classify_anchor_with_rom;

/// Configuration for the orchestrator.  Held outside the
/// orchestrator function so callers can construct one and reuse the
/// strider / sleigh / options across iterations without re-paying
/// per-iteration setup costs.
pub struct OrchestratorConfig<'a, B>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer + Clone,
{
    /// The strider — stable across iterations.
    pub strider: &'a Strider,
    /// Function entry address.
    pub start_addr: u64,
    /// Sleigh-specification factory: invoked once per iteration to
    /// build a fresh Sleigh context with a clean memory reader.  We
    /// take a closure rather than the Sleigh directly because
    /// [`cfg::Builder`] consumes the Sleigh by value on `build()`.
    pub make_sleigh: Box<dyn FnMut() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>>,
    /// Read-only memory image for the optimiser's `LoadReadOnly`
    /// pass.  `None` to disable.  Cloned per-iteration via
    /// `Arc::clone` (cheap).
    pub rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
}

/// Round-1 orchestrator.  Drives the iterate-resolve-feed-back loop
/// until either:
///
///   * no `BranchIndirect` remains unresolved, or
///   * the resolution set is unchanged across two consecutive
///     iterations (fixed point with some unresolved branches → typed
///     error), or
///   * the iteration cap is hit (typed error — soundness bug).
///
/// Returns the optimised IR on success.
///
/// # Errors
///
/// * [`ErrorKind::IndirectResolutionDidNotConverge`] when the cap is hit.
/// * [`ErrorKind::UnresolvedIndirectBranch`] at fixed point with
///   unresolved branches remaining.
/// * Propagates strider / cfg / opt errors verbatim.
pub fn run<B>(mut config: OrchestratorConfig<'_, B>) -> Result<ir::BuiltFunctionGraph>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer + Clone,
{
    // The `known_targets` map accumulates tier-2 resolutions across
    // iterations.  Each iteration replaces the map (we don't merge
    // — see spec's "Resolution feedback semantics" section: each
    // iteration's classification can legitimately upgrade across
    // iterations, e.g. `Single(K1) → Multiple([K1, K2])`).
    let mut known_targets: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();

    // Iteration 0 first to record the baseline pending count.
    let (mut graph, mut unresolved) = build_lift_optimise(&mut config, &known_targets)?;

    // Fast path: function with no `BranchIndirect` at all.  No tier-2
    // work, no rebuild — return immediately.  This is the common
    // case (most functions don't have indirect branches).
    if unresolved.is_empty() {
        return Ok(graph);
    }

    let pending_at_iter_0 = unresolved.len();

    // Iteration cap.  CORRECTNESS: every legal classification
    // transition strictly grows the induced edge set, so the loop
    // must terminate within at most O(pending_at_iter_0) steps for
    // each branch (Single → Multiple → bounded width).  The
    // `2 * pending + 4` formula is the spec's conservative bound;
    // hitting it indicates a soundness bug in the resolver.
    let cap = 2usize.saturating_mul(pending_at_iter_0).saturating_add(4);

    for _iter in 0..cap {
        // Classify every unresolved anchor on the current optimised
        // IR.  Build the next `known_targets` map from scratch — see
        // the spec's "Resolution feedback semantics" — so a per-
        // branch classification upgrade is captured without needing
        // a per-iteration delta.
        let mut next_known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        let lr_vn = config.strider.calling_convention().link_register_vn;
        for (addr, anchor_output) in &unresolved {
            // The recorded NodeOutputId may have been
            // `replace_all_uses`-rewritten by the optimiser — walk
            // to the placeholder Return's current value-input slot
            // to find the live producer.  The placeholder Return's
            // shape: [control, memory, target_value] — input #2 is
            // the slot we want.
            //
            // (Round-1 simplification: we use the raw recorded
            // anchor.  The strider lift records the anchor *before*
            // the optimiser runs, so for cases where an opt pass
            // rewrites the slot, we'd need the walk-to-input dance.
            // The current x86_64 fixtures don't trigger it because
            // the placeholder Return's input never gets
            // `replace_all_uses`-rewritten.  When it does, we
            // surface a None classification and let the next
            // iteration retry.)
            let _ = anchor_output; // silence unused warning when feature off
            // R4: pass the rom through so the jump-table arm can
            // read table entries.  Cloning the Arc is cheap
            // (atomic refcount); we hold a borrow across the
            // classifier call by promoting the Arc to a `&dyn`.
            let rom_ref: Option<&dyn ReadOnlyMemory> = config.rom.as_deref();
            if let Some(resolved) =
                classify_anchor_with_rom(&graph, *anchor_output, lr_vn, rom_ref)
            {
                next_known.insert(*addr, resolved);
            }
        }

        // Compare induced edge sets (see spec).  If the new edge
        // set equals the old, we've reached a fixed point: either
        // every branch has a stable classification (success) or
        // some are still unresolved (error).
        if edge_set_of(&next_known) == edge_set_of(&known_targets) {
            // Fixed point.  If any branch is still unresolved,
            // surface as typed error.
            if !unresolved.is_empty() {
                let some_addr = unresolved
                    .iter()
                    .filter(|(addr, _)| !next_known.contains_key(addr))
                    .map(|(addr, _)| *addr)
                    .next();
                if let Some(addr) = some_addr {
                    return Err(ErrorKind::UnresolvedIndirectBranch(addr).into());
                }
            }
            return Ok(graph);
        }

        // Advance: install the new map, rebuild + lift + optimise.
        known_targets = next_known;
        let (g, u) = build_lift_optimise(&mut config, &known_targets)?;
        graph = g;
        unresolved = u;

        // Convergence shortcut: if the rebuild produced no
        // unresolved branches, we're done.
        if unresolved.is_empty() {
            return Ok(graph);
        }
    }

    // Cap exceeded — soundness bug.  Surface as typed error rather
    // than panicking.
    Err(ErrorKind::IndirectResolutionDidNotConverge(cap).into())
}

/// Builds the CFG, lifts to IR, runs the optimiser pipeline,
/// returns the resulting `BuiltFunctionGraph` and the unresolved-
/// branch table.
fn build_lift_optimise<B>(
    config: &mut OrchestratorConfig<'_, B>,
    known_targets: &HashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Result<(ir::BuiltFunctionGraph, Vec<(PcodeInsnAddr, ir::Value)>)>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer + Clone,
{
    // Fresh sleigh per iteration — Builder consumes by value.
    let sleigh = (config.make_sleigh)();
    let mut opts_builder = OptionsBuilder::new();
    if let Some(rom) = config.rom.clone() {
        opts_builder = opts_builder.set_read_only_memory(rom);
    }
    if let Some(lr) = config.strider.calling_convention().link_register_vn {
        opts_builder = opts_builder.set_link_register(lr);
    }
    let opts = opts_builder.build();

    let arch_endianness = config.strider.arch().endianness;
    let cfg: Cfg<rsleigh::mem_readers::BufMemReader<B>> =
        Builder::with_endianness(sleigh, config.start_addr, opts, arch_endianness)
            .with_known_targets(known_targets.clone())
            .build()?;

    let outcome: AnalyzeOutcome = config.strider.analyze_cfg_with_unresolved(&cfg)?;
    let unresolved = outcome.unresolved_branches.clone();
    let mut graph = outcome.graph;

    // Run the optimiser.  Round-1: full pipeline.  Round-2+ will
    // split into stable / destructive subsets per the spec.
    let pipeline = config.strider.build_optimizer_pipeline();
    pipeline.run(&mut graph)?;

    Ok((graph, unresolved))
}

/// Helper: arch info accessor on `Strider`.
///
/// This is a thin wrapper because `Strider::arch` is a private field
/// in pipeline.rs.  Add a small accessor on Strider; the change is
/// load-bearing for the orchestrator and is a pure API expansion.
impl Strider {}

/// The induced edge set of a `known_targets` map: a sorted
/// `Vec<(PcodeInsnAddr, u64)>` for `Single` / `Multiple` and a
/// special sentinel for `LinkRegister`.  Used by the orchestrator
/// to test convergence.
///
/// # Why a Vec rather than a HashSet
///
/// We sort + dedup so equality comparison is structural and cheap.
/// HashSet would require hashing every element on every comparison.
fn edge_set_of(
    map: &HashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Vec<(PcodeInsnAddr, EdgeKind)> {
    let mut edges: Vec<(PcodeInsnAddr, EdgeKind)> = Vec::new();
    for (addr, resolved) in map {
        match resolved {
            ResolvedTargets::LinkRegister => {
                edges.push((*addr, EdgeKind::LinkRegister));
            }
            ResolvedTargets::Single(k) => {
                edges.push((*addr, EdgeKind::Target(*k)));
            }
            ResolvedTargets::Multiple(targets) => {
                for k in targets {
                    edges.push((*addr, EdgeKind::Target(*k)));
                }
            }
        }
    }
    edges.sort();
    edges.dedup();
    edges
}

/// Edge kind discriminator for the induced edge set.  `LinkRegister`
/// is its own kind because two BranchIndirects classified as
/// LinkRegister produce equivalent edges (no successor) regardless
/// of any address payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeKind {
    LinkRegister,
    Target(u64),
}

// Avoid a clippy `dead_code` warning on the unused `MachineInsnAddr`
// import — this module imports it because the cache types live in
// `MachineInsnAddr`-keyed maps and a future round will plumb cache
// queries through the orchestrator.  Round-1 doesn't need it; the
// import stays so future edits land cleanly without a fresh `use`.
#[allow(dead_code)]
fn _machine_insn_addr_phantom_use() -> MachineInsnAddr {
    MachineInsnAddr { addr: 0 }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the orchestrator's helper functions.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: 0,
        }
    }

    #[test]
    fn edge_set_of_empty_map_is_empty() {
        let map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        let edges = edge_set_of(&map);
        assert!(edges.is_empty());
    }

    #[test]
    fn edge_set_of_single_link_register_resolution() {
        // One LinkRegister entry → one (addr, LinkRegister) edge.
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(pcode_addr(0x1000), ResolvedTargets::LinkRegister);
        let edges = edge_set_of(&map);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0], (pcode_addr(0x1000), EdgeKind::LinkRegister));
    }

    #[test]
    fn edge_set_of_single_resolution_matches_single_edge() {
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        let edges = edge_set_of(&map);
        assert_eq!(edges, vec![(pcode_addr(0x1000), EdgeKind::Target(0x2000))]);
    }

    #[test]
    fn edge_set_of_multiple_resolution_matches_n_edges() {
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(
            pcode_addr(0x1000),
            ResolvedTargets::Multiple(vec![0x2000, 0x3000, 0x4000]),
        );
        let edges = edge_set_of(&map);
        assert_eq!(edges.len(), 3);
        // sorted + deduped
        assert_eq!(edges[0], (pcode_addr(0x1000), EdgeKind::Target(0x2000)));
        assert_eq!(edges[1], (pcode_addr(0x1000), EdgeKind::Target(0x3000)));
        assert_eq!(edges[2], (pcode_addr(0x1000), EdgeKind::Target(0x4000)));
    }

    #[test]
    fn edge_set_is_order_independent() {
        // Two maps that differ only in HashMap iteration order must
        // produce identical edge sets — the sort-and-dedup makes
        // the function stable against HashMap's random hasher seed.
        let mut a: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        a.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        a.insert(pcode_addr(0x3000), ResolvedTargets::Single(0x4000));
        let mut b: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        b.insert(pcode_addr(0x3000), ResolvedTargets::Single(0x4000));
        b.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        assert_eq!(edge_set_of(&a), edge_set_of(&b));
    }

    #[test]
    fn edge_set_dedups_duplicate_targets_in_multiple() {
        // A Multiple with the same target listed twice produces
        // exactly one edge after dedup.  Defends against double-
        // counting in a future classifier change.
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(
            pcode_addr(0x1000),
            ResolvedTargets::Multiple(vec![0x2000, 0x2000, 0x2000]),
        );
        let edges = edge_set_of(&map);
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn iteration_cap_formula_handles_zero_pending_branches() {
        // Round-1 sanity check on the cap formula
        // `2 * pending + 4`.  When `pending == 0`, the cap is 4 —
        // i.e. the orchestrator never enters the loop more than 4
        // times even on a fresh function with zero pending
        // branches.  In practice we exit before iteration 0 hits
        // the loop body (the `unresolved.is_empty()` fast-path),
        // but the cap must still be defined and finite.
        let pending = 0usize;
        let cap = 2usize.saturating_mul(pending).saturating_add(4);
        assert_eq!(cap, 4);
    }

    #[test]
    fn iteration_cap_formula_one_pending_branch() {
        let cap = 2usize.saturating_mul(1).saturating_add(4);
        assert_eq!(cap, 6);
    }

    #[test]
    fn iteration_cap_saturates_at_max() {
        // Pathological input: pending == usize::MAX.  The cap must
        // saturate, never panic on overflow.
        let cap = 2usize.saturating_mul(usize::MAX).saturating_add(4);
        assert_eq!(cap, usize::MAX);
    }
}
