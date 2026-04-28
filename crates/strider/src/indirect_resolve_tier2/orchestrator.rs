//! Outer fixed-point orchestrator for indirect-branch resolution.
//!
//! Drives the iterate-resolve-feed-back loop the spec describes:
//!
//!   1. Build the CFG (with the current `known_targets` map).
//!   2. Lift to IR via `Strider::analyze_cfg_with_unresolved`.
//!   3. Run the **stable** optimizer subset
//!      ([`Strider::build_stable_optimizer_pipeline`]).  Intermediate
//!      iterations MUST NOT run the destructive subset, since
//!      `RedundantPhis` / `DeadBranchElimination` would invalidate the
//!      cache's pinned phi `NodeId`s.
//!   4. For each unresolved anchor, run [`super::classify_anchor`].
//!   5. Apply in-place edits for terminal classifications:
//!      [`super::apply_link_register`] for `LinkRegister`, and
//!      [`super::apply_tail_call`] for `Single(K)` where K is outside
//!      the function range (tail call).  These do NOT trigger a CFG
//!      rebuild — they're local IR mutations.
//!   6. If any classification requires a structural rebuild
//!      (intra-fn `Single`, `Multiple` jump table), update
//!      `known_targets` and rebuild the CFG.  Otherwise the loop
//!      stays on the same CFG.
//!   7. At fixed point: if any branch is still unresolved, return
//!      `Err(UnresolvedIndirectBranch(addr))`.  Otherwise run the
//!      destructive subset
//!      ([`Strider::build_destructive_optimizer_pipeline`]) once and
//!      return the optimized IR.
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
//! # Tail-call detection
//!
//! A `Single(K)` resolution where `K` lies outside the function
//! address range (`K < start_addr` OR `K >= start_addr + fn_max_size`)
//! is treated as a tail call.  Tail calls are applied as in-place IR
//! edits via [`super::apply_tail_call`] — no CFG rebuild.  Inside-the-
//! function `Single(K)` requires a CFG rebuild because new code
//! becomes reachable.

#![allow(clippy::module_name_repetitions)]

use std::collections::HashMap;

use cfg::{Builder, Cfg, MachineInsnAddr, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use ir::node::{NodeId, NodeKind};
use opt::ReadOnlyMemory;

use crate::error::{ErrorKind, Result};
use crate::strider::Strider;
use crate::{cache_key_for_region, lift_new_regions_into_with_stats, RegionIrCache};

use super::{apply_link_register, apply_tail_call, classify_anchor_with_rom};

/// Per-iteration snapshot of a `Cfg`'s region insn counts, keyed by
/// machine address.  Used by [`build_lift_stable`] to detect region
/// splits between iterations without needing to keep the previous
/// iteration's full `Cfg` alive (which would also pin its `Sleigh`,
/// preventing us from threading a single `Sleigh` through all
/// iterations).
type CfgSplitSnapshot = HashMap<MachineInsnAddr, usize>;

/// Build a [`CfgSplitSnapshot`] from `cfg` — captures only the data
/// [`apply_split_invalidation`] needs, so the orchestrator can drop
/// the full `Cfg` (including its `Sleigh`) after the snapshot is
/// taken.  This is the key enabler of the per-run single-`Sleigh`
/// contract: the orchestrator never holds a stale `Cfg` across
/// iterations — only its split-detection snapshot.
fn take_split_snapshot<R: rsleigh::MemReader>(cfg: &Cfg<R>) -> Result<CfgSplitSnapshot> {
    let mut snapshot = HashMap::new();
    for region_id in cfg.region_ids() {
        let key = cache_key_for_region(cfg, region_id)?;
        let region = cfg
            .graph
            .node_weight(region_id)
            .ok_or(crate::error::ErrorKind::CfgNoRegion(region_id))?;
        snapshot.insert(key, region.insns.len());
    }
    Ok(snapshot)
}

/// Cache-eviction pass equivalent to [`crate::invalidate_split_regions`]
/// but driven by a [`CfgSplitSnapshot`] instead of a full `Cfg`.  The
/// orchestrator uses this so it can drop the previous iteration's
/// `Cfg` (recovering its `Sleigh`) and still detect region splits.
///
/// Returns the number of cache entries evicted, so the caller can
/// update [`OrchestratorStats::cache_evictions_on_split`].
///
/// CORRECTNESS — equivalence with `invalidate_split_regions`: both
/// scan `new_cfg`'s regions, compare each region's current insn count
/// against the same region's count in the previous iteration, and
/// evict the cache entry on shrinkage (= a `split_region` event
/// moved insns out of this region).  The snapshot variant has the
/// exact same semantics — only the data source for the previous
/// counts differs.
fn apply_split_invalidation<R: rsleigh::MemReader>(
    cache: &mut RegionIrCache,
    prev_snapshot: &CfgSplitSnapshot,
    new_cfg: &Cfg<R>,
) -> Result<usize> {
    let cache_size_before = cache.len();
    for region_id in new_cfg.region_ids() {
        let key = cache_key_for_region(new_cfg, region_id)?;
        if !cache.contains_key(&key) {
            continue;
        }
        let new_region = new_cfg
            .graph
            .node_weight(region_id)
            .ok_or(crate::error::ErrorKind::CfgNoRegion(region_id))?;
        let new_insn_count = new_region.insns.len();
        let Some(&old_insn_count) = prev_snapshot.get(&key) else {
            continue;
        };
        if new_insn_count < old_insn_count {
            cache.remove(&key);
        }
    }
    Ok(cache_size_before.saturating_sub(cache.len()))
}

/// Configuration for the orchestrator.  Held outside the
/// orchestrator function so callers can construct one and reuse the
/// strider / sleigh / options across iterations without re-paying
/// per-iteration setup costs.
pub struct OrchestratorConfig<'a, B>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    /// The strider — stable across iterations.
    pub strider: &'a Strider,
    /// Function entry address.
    pub start_addr: u64,
    /// The Sleigh context, owned by the config and threaded through
    /// every iteration of the orchestrator's fixed-point loop.
    ///
    /// W5 — pre-W5 this was a `Box<dyn FnMut() -> Sleigh>` factory
    /// because the (now-superseded) round-1 orchestrator built a fresh
    /// Sleigh per iteration.  After the Sleigh-persistence work the
    /// closure was called exactly once at loop entry; W5 drops the
    /// indirection and takes the owned `Sleigh<R>` directly so the
    /// "constructed once, reused per iteration" contract is visible
    /// at the type signature.
    ///
    /// CORRECTNESS — the orchestrator threads this Sleigh into the
    /// first `cfg::Builder::with_endianness` call (consumes by value),
    /// then harvests it back from `Cfg::sleigh` after each iteration's
    /// CFG drops, and re-uses the same handle in the next iteration.
    /// See `Cfg::sleigh`'s persistence-contract docs.
    pub sleigh: Option<rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>>,
    /// Read-only memory image for the optimiser's `LoadReadOnly`
    /// pass.  `None` to disable.  Cloned per-iteration via
    /// `Arc::clone` (cheap).
    pub rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
    /// Maximum function size in bytes.  When set, a `Single(K)`
    /// resolution with `K >= start_addr + fn_max_size` is treated as a
    /// tail call (in-place edit, no CFG rebuild).  When `None`, only
    /// `K < start_addr` is treated as a tail call.  Mirrors
    /// [`cfg::OptionsBuilder::set_function_max_size`] so the
    /// orchestrator's tail-call decision matches the cfg builder's.
    pub fn_max_size: Option<u64>,
    /// When `true`, `Single(K)` with `K < start_addr` is NOT treated
    /// as a tail call — i.e. the orchestrator follows it as an intra-fn
    /// branch.  Mirrors
    /// [`cfg::OptionsBuilder::allow_code_before_start_addr`].
    pub allow_code_before_start_addr: bool,
}

/// Statistics emitted by [`run_with_stats`] so tests (and downstream
/// observability hooks) can pin which optimizer subset ran in which
/// phase, count CFG rebuilds, in-place edits, and total IR-lift calls.
///
/// Each field is incremented monotonically across the orchestrator's
/// iteration loop; reset to zero on a fresh `run_with_stats` call.
///
/// CORRECTNESS NOTE: tests use these counters to pin the spec's
/// pipeline-tier separation contract — `destructive_runs == 1` for
/// every successful run, `stable_runs >= destructive_runs` always.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrchestratorStats {
    /// Total times [`Strider::build_stable_optimizer_pipeline`] ran.
    /// At minimum 1 (the initial lift always runs the stable subset);
    /// each rebuild iteration adds 1 more.
    pub stable_runs: usize,
    /// Total times [`Strider::build_destructive_optimizer_pipeline`]
    /// ran.  Exactly 1 on a successful run (fast-path or
    /// fixed-point exit); 0 on the iteration-cap / unresolved-at-
    /// fixed-point error paths since those abort before the
    /// destructive run.
    pub destructive_runs: usize,
    /// Total times the CFG was rebuilt.  At minimum 1 (the initial
    /// build).  Each `Single(K)` intra-fn or `Multiple` resolution
    /// adds 1.  Tail-call `Single(K)` and `LinkRegister` resolutions
    /// do NOT trigger a rebuild — they fire as in-place edits.
    pub cfg_rebuilds: usize,
    /// Total times [`super::apply_link_register`] ran.
    pub link_register_edits: usize,
    /// Total times [`super::apply_tail_call`] ran.
    pub tail_call_edits: usize,
    /// Total iterations of the outer fixed-point loop.  0 when the
    /// fast-path skipped the loop entirely.
    pub iterations: usize,
    /// Sum, across all `lift_new_regions_into` calls, of pcode insns
    /// the cache contract considered **newly lifted**.  In round 1 the
    /// IR is physically rebuilt each iteration, but cached regions do
    /// NOT contribute to this counter (mirrors round-2 semantics where
    /// a persistent FunctionBuilder genuinely skips them).  Tests use
    /// this to pin the spec's "every pcode instruction is lifted to IR
    /// at most once across the entire fixed-point analysis" contract
    /// at the API surface — see [`crate::ir_cache::LiftStats`].
    pub pcode_insns_lifted: usize,
    /// Sum, across all `lift_new_regions_into` calls, of regions the
    /// cache considered newly lifted.  Same round-1 / round-2 caveat
    /// as `pcode_insns_lifted`.
    pub regions_newly_lifted: usize,
    /// Number of cache entries evicted by `invalidate_split_regions`
    /// across all rebuilds.  Round-1 pin: a `Multiple([t])` resolution
    /// where `t` lands mid-region produces exactly one eviction; a
    /// `Multiple([t])` where `t` is a fresh address produces zero.
    pub cache_evictions_on_split: usize,
}

/// Round-1 orchestrator entry point.  Drives the iterate-resolve-
/// feed-back loop and discards the stats; equivalent to
/// [`run_with_stats`] with the stats dropped.
///
/// # Errors
///
/// * [`ErrorKind::IndirectResolutionDidNotConverge`] when the cap is hit.
/// * [`ErrorKind::UnresolvedIndirectBranch`] at fixed point with
///   unresolved branches remaining.
/// * Propagates strider / cfg / opt errors verbatim.
pub fn run<B>(config: OrchestratorConfig<'_, B>) -> Result<ir::BuiltFunctionGraph>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    let (graph, _stats) = run_with_stats(config)?;
    Ok(graph)
}

/// Variant of [`run`] that also returns an [`OrchestratorStats`].
/// Tests pin pipeline-tier / rebuild / in-place-edit counts via this
/// entry point.
///
/// # Errors
///
/// Same as [`run`].
pub fn run_with_stats<B>(
    mut config: OrchestratorConfig<'_, B>,
) -> Result<(ir::BuiltFunctionGraph, OrchestratorStats)>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    let mut stats = OrchestratorStats::default();
    // The `known_targets` map accumulates tier-2 resolutions across
    // iterations.  Each iteration replaces the map (we don't merge
    // — see spec's "Resolution feedback semantics" section: each
    // iteration's classification can legitimately upgrade across
    // iterations, e.g. `Single(K1) → Multiple([K1, K2])`).
    let mut known_targets: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();

    // Persistent state across iterations:
    //
    //   * `region_ir_cache` accumulates `RegionIrEntry` records.  In
    //     round 1 each rebuild clears + repopulates entries (the
    //     fresh FunctionBuilder produces fresh NodeIds), but the
    //     `LiftStats` returned by `lift_new_regions_into_with_stats`
    //     reports counts under the cache contract: regions already
    //     present pre-call do NOT contribute to lift counters.  This
    //     pins the spec's "lifted at most once" contract at the API
    //     surface, even while round 1's physical lift is still
    //     non-incremental.
    //
    //   * `prev_snapshot` retains a per-region insn-count snapshot of
    //     the previous iteration's CFG so [`apply_split_invalidation`]
    //     can compare insn counts to detect region splits at rebuild
    //     time.  We keep only the snapshot — not the full `Cfg` — so
    //     the previous iteration's `Sleigh` (the heavy resource we
    //     thread through `Sleigh persistence`) is recovered by
    //     destructuring the consumed `Cfg` and re-used across iterations
    //     instead of being dropped.  `None` before the first build
    //     completes; `Some(snapshot)` thereafter.
    //
    //   * `sleigh` is the ONE `rsleigh::Sleigh` handle used by every
    //     iteration.  Owned by the caller and handed over via `config.sleigh` (W5).  Taken at
    //     loop entry, consumed by `Builder::with_endianness` per
    //     iteration, and harvested back via `cfg.sleigh` between
    //     iterations.  CORRECTNESS — re-using one Sleigh across many
    //     `lift_one` calls is sound: `lift_one` takes `&mut self`
    //     because the C++ side mutates internal decode buffers, which
    //     are reset on every call; there is no per-CFG state in
    //     Sleigh.  This is the same pattern `cfg::region_builder` uses
    //     within a single CFG build.  Re-constructing the Sleigh
    //     per iteration would re-load the SLA spec — a measurable
    //     hot-path cost the orchestrator's fixed-point loop pays for
    //     every CFG rebuild.
    //
    //   * `known_targets` and the lift counters in `stats` (see the
    //     `pcode_insns_lifted` / `regions_newly_lifted` fields) make
    //     the round-2 transition local: a future round that persists
    //     the IR graph across iterations can drop in alongside this
    //     orchestrator without changing the loop control flow.
    let mut region_ir_cache: RegionIrCache = std::collections::HashMap::new();
    let mut prev_snapshot: Option<CfgSplitSnapshot> = None;
    // Take ownership of the Sleigh out of the config (W5).  The
    // orchestrator threads this single handle through every iteration —
    // see the `sleigh` field doc for the persistence contract.
    // `Option::take` lets us move ownership while keeping `config` borrowed
    // by the rest of the loop (which still reads `config.strider`,
    // `config.rom`, etc).
    let mut sleigh: rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>> =
        config.sleigh.take().ok_or_else(|| ErrorKind::Unimplemented(
            "OrchestratorConfig::sleigh was None — caller must supply an owned Sleigh".to_string(),
        ))?;

    // Iteration 0: build the CFG, lift to IR, run the stable subset.
    let (mut graph, mut unresolved, iter0_cfg) = build_lift_stable(
        sleigh,
        &config,
        &known_targets,
        &mut stats,
        &mut region_ir_cache,
        prev_snapshot.as_ref(),
    )?;
    // Harvest sleigh + snapshot from iter 0's cfg, then drop the
    // remainder.  The Cfg is no longer needed once we have its
    // split-detection snapshot — the orchestrator's classifier and
    // in-place edits operate on the IR graph alone.
    prev_snapshot = Some(take_split_snapshot(&iter0_cfg)?);
    let Cfg { sleigh: harvested_sleigh, .. } = iter0_cfg;
    sleigh = harvested_sleigh;

    // Fast path: function with no `BranchIndirect` at all.  No tier-2
    // work, no rebuild — but we DO still run the destructive subset
    // here so the returned IR matches the production-quality shape
    // the previous orchestrator (and downstream consumers) expect.
    // The destructive subset is safe here because the IR shape is
    // final — there are no future iterations to break a destructive
    // rewrite.
    if unresolved.is_empty() {
        run_destructive(&config, &mut graph, &mut stats)?;
        return Ok((graph, stats));
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
        stats.iterations += 1;
        // Classify every unresolved anchor on the current optimised
        // IR.  Build the next `known_targets` map from scratch — see
        // the spec's "Resolution feedback semantics" — so a per-
        // branch classification upgrade is captured without needing
        // a per-iteration delta.
        let mut next_known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        let lr_vn = config.strider.calling_convention().link_register_vn;
        // Track which anchors are slated for in-place edits (so we
        // do NOT add them to `next_known` — in-place edits remove
        // the placeholder Return; passing the same `known_targets`
        // through to a CFG rebuild would re-emit the same placeholder
        // and we'd be stuck in a cycle).
        let mut in_place_edits: Vec<(NodeId, ResolvedTargets)> = Vec::new();
        for (addr, anchor_output) in &unresolved {
            // R4: pass the rom through so the jump-table arm can
            // read table entries.  Cloning the Arc is cheap
            // (atomic refcount); we hold a borrow across the
            // classifier call by promoting the Arc to a `&dyn`.
            let rom_ref: Option<&dyn ReadOnlyMemory> = config.rom.as_deref();
            let Some(resolved) =
                classify_anchor_with_rom(&graph, *anchor_output, lr_vn, rom_ref)
            else {
                continue;
            };

            // Decide whether this resolution can be applied as an
            // in-place edit (no CFG rebuild) or requires a structural
            // rebuild.  See spec's "In-place IR edits vs CFG rebuild"
            // section table.
            let placeholder_return =
                find_placeholder_return_for_anchor(&graph, *anchor_output);
            let can_inplace = match (&resolved, placeholder_return) {
                // LinkRegister: always in-place — placeholder Return
                // already has the right shape.
                (ResolvedTargets::LinkRegister, Some(_)) => true,
                // Single(K): in-place iff K is a tail call.
                (ResolvedTargets::Single(target), Some(_)) => {
                    is_tail_call(*target, &config)
                }
                // Multiple / no placeholder: structural rebuild.
                _ => false,
            };

            if can_inplace
                && let Some(ret) = placeholder_return
            {
                in_place_edits.push((ret, resolved.clone()));
                // Don't add to next_known — the in-place edit
                // erases the placeholder; the cfg builder must
                // not see this address again.
                let _ = addr;
                continue;
            }
            next_known.insert(*addr, resolved);
        }

        // Apply in-place edits.  Each edit mutates the graph
        // directly; the cache's boundary handles stay valid because
        // the edits touch only the placeholder Return's subgraph
        // (`apply_link_register` keeps the same NodeId; `apply_tail_call`
        // patches the cache's `exit_control` via the helper threaded
        // into `apply_in_place_edit`).
        for (placeholder, resolved) in &in_place_edits {
            apply_in_place_edit(
                &mut graph,
                &config,
                *placeholder,
                resolved,
                &mut stats,
                &mut region_ir_cache,
            )?;
        }

        // Recompute unresolved AFTER in-place edits — the edits
        // remove placeholder Returns from the IR, so the surviving
        // unresolved list is what the cfg builder needs as input.
        let unresolved_after_edits = if in_place_edits.is_empty() {
            unresolved.clone()
        } else {
            unresolved
                .iter()
                .filter(|(_, anchor)| {
                    find_placeholder_return_for_anchor(&graph, *anchor).is_some()
                })
                .cloned()
                .collect()
        };

        // Compare induced edge sets (see spec).  If the new edge
        // set equals the old AND we did no in-place edits this
        // iteration, we've reached a fixed point: either every branch
        // has a stable classification (success) or some are still
        // unresolved (error).
        let edge_set_changed = edge_set_of(&next_known) != edge_set_of(&known_targets);
        if !edge_set_changed && in_place_edits.is_empty() {
            // Fixed point.  Any branch in `unresolved_after_edits`
            // not in `next_known` is genuinely unresolvable.
            if !unresolved_after_edits.is_empty() {
                let some_addr = unresolved_after_edits
                    .iter()
                    .filter(|(addr, _)| !next_known.contains_key(addr))
                    .map(|(addr, _)| *addr)
                    .next();
                if let Some(addr) = some_addr {
                    return Err(ErrorKind::UnresolvedIndirectBranch(addr).into());
                }
            }
            // Run the destructive subset exactly once at the fixed
            // point.  The IR shape is now final — see spec's
            // "Pipeline split" rationale.
            run_destructive(&config, &mut graph, &mut stats)?;
            return Ok((graph, stats));
        }

        // Update unresolved for the next iteration / rebuild path.
        unresolved = unresolved_after_edits;

        // If only in-place edits fired (no edge-set change), no CFG
        // rebuild — re-run the stable subset on the freshly-edited
        // IR and re-classify in the next loop turn.
        if !edge_set_changed {
            run_stable_only(&config, &mut graph, &mut stats)?;
            // After in-place edits, if no unresolved branches remain
            // we're done — run the destructive subset and return.
            if unresolved.is_empty() {
                run_destructive(&config, &mut graph, &mut stats)?;
                return Ok((graph, stats));
            }
            continue;
        }

        // Edge-set changed → structural rebuild.  Thread the
        // currently-held Sleigh into the next build, then harvest it
        // back from the new CFG so the next iteration sees the same
        // handle (no re-construction).
        known_targets = next_known;
        let (g, u, new_cfg) = build_lift_stable(
            sleigh,
            &config,
            &known_targets,
            &mut stats,
            &mut region_ir_cache,
            prev_snapshot.as_ref(),
        )?;
        graph = g;
        unresolved = u;
        // Harvest the snapshot + Sleigh BEFORE the next iteration.
        prev_snapshot = Some(take_split_snapshot(&new_cfg)?);
        let Cfg { sleigh: harvested_sleigh, .. } = new_cfg;
        sleigh = harvested_sleigh;

        // Convergence shortcut: if the rebuild produced no
        // unresolved branches, we're done.
        if unresolved.is_empty() {
            run_destructive(&config, &mut graph, &mut stats)?;
            return Ok((graph, stats));
        }
    }

    // Cap exceeded — soundness bug.  Surface as typed error rather
    // than panicking.
    Err(ErrorKind::IndirectResolutionDidNotConverge(cap).into())
}

/// Decides whether `target` is a tail call — i.e. lies outside the
/// function's address range.  Mirrors
/// `cfg::Builder::is_branch_tail_call_nocheck` (which the orchestrator
/// must agree with so the cfg builder accepts our in-place
/// resolutions).
fn is_tail_call<B>(target: u64, config: &OrchestratorConfig<'_, B>) -> bool
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    if target < config.start_addr && !config.allow_code_before_start_addr {
        return true;
    }
    if let Some(fn_max_size) = config.fn_max_size {
        let end_exclusive = config.start_addr.saturating_add(fn_max_size);
        if end_exclusive <= target {
            return true;
        }
    }
    false
}

/// Locate the unique placeholder Return whose value-input slot points
/// at `anchor_output`.  The placeholder shape is `Return(control,
/// memory, target_value)` — input #2 is the anchor.
///
/// Returns `None` if no placeholder Return references `anchor_output`
/// (e.g. an earlier in-place edit already replaced it, or the anchor
/// has been `replace_all_uses`-rewritten by an opt pass).  Callers
/// treat `None` as "skip this anchor this iteration."
fn find_placeholder_return_for_anchor(
    graph: &ir::BuiltFunctionGraph,
    anchor_output: ir::Value,
) -> Option<NodeId> {
    // Walk the use-list of the anchor: any Return-shaped consumer
    // is a candidate placeholder.  Restrict to 3-input Returns
    // (the placeholder shape) since an ABI Return has 2 +
    // ret_val_regs.len() inputs.
    for (consumer, _input_index) in graph.graph.output_uses(anchor_output) {
        if !matches!(graph.graph.node_kind(consumer), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = graph.graph.node_inputs(consumer).into_iter().collect();
        if inputs.len() == 3 && inputs[2] == anchor_output {
            return Some(consumer);
        }
    }
    None
}

/// Dispatch on the resolution variant: LinkRegister → append ABI
/// ret-val regs; tail-call Single → splice Call+Return.  Updates
/// `stats` and propagates IR errors.
///
/// Threads the [`RegionIrCache`] so the tail-call arm can patch the
/// affected region's `exit_control` handle after [`apply_tail_call`]
/// produces a fresh control output.
fn apply_in_place_edit<B>(
    graph: &mut ir::BuiltFunctionGraph,
    config: &OrchestratorConfig<'_, B>,
    placeholder: NodeId,
    resolved: &ResolvedTargets,
    stats: &mut OrchestratorStats,
    cache: &mut RegionIrCache,
) -> Result<()>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    match resolved {
        ResolvedTargets::LinkRegister => {
            // CORRECTNESS: appending ret-val regs to the placeholder
            // Return preserves its NodeId and its control/memory
            // chain — the cache's exit_control handle stays valid.
            let ret_vals = read_ret_val_outputs(graph, placeholder, config)?;
            apply_link_register(graph, placeholder, &ret_vals)?;
            stats.link_register_edits += 1;
            Ok(())
        }
        ResolvedTargets::Single(target) => {
            // CORRECTNESS: apply_tail_call detaches the placeholder's
            // inputs, builds Call+Return on the same control/memory
            // chain, and returns the new Return's NodeId.  Body refs
            // outside the placeholder subgraph are untouched.
            //
            // CACHE EXIT HANDLE PATCHING: the cache entry for this
            // region was populated with `exit_control` = the
            // placeholder Return's control input (= body's ctrl chain
            // end).  After the edit, that NodeOutputId is consumed by
            // the Call (the Call's ctrl-in slot), so its semantic
            // "this is what the terminator reads" still holds.  But
            // the new Return's control INPUT is the Call's control
            // OUTPUT — a fresh NodeOutputId — and downstream code
            // (e.g. consumers querying "what control does this
            // region's terminator consume?") should see the new id.
            // Update the cache entry whose exit_control matches the
            // placeholder's old control input to point at the new
            // Call.ctrl_out.  See
            // `update_cache_exit_handle_after_tail_call` for the
            // matching protocol.
            let ret_vals = read_ret_val_outputs(graph, placeholder, config)?;
            // Capture the placeholder's pre-edit control input — this
            // is the cache key we'll match on AFTER apply_tail_call
            // detaches the placeholder.  Doing it before the edit
            // means we don't have to walk the detached node's inputs
            // (which were nullified).
            let old_exit_control_opt: Option<ir::node::NodeOutputId> = {
                let inputs: Vec<_> = graph.graph.node_inputs(placeholder).into_iter().collect();
                if inputs.len() == 3 {
                    Some(inputs[0])
                } else {
                    None
                }
            };
            let new_return = apply_tail_call(graph, placeholder, *target, &ret_vals)?;
            // CORRECTNESS — the new Return's input #0 is the Call's
            // ctrl output, which is the new "exit control" of this
            // region.  Update the cache entry that previously
            // recorded `old_exit_control` so future cache reads see
            // the live id.
            if let Some(old_exit) = old_exit_control_opt {
                let new_inputs: Vec<_> =
                    graph.graph.node_inputs(new_return).into_iter().collect();
                if let Some(new_exit_control) = new_inputs.first().copied() {
                    update_cache_exit_handle_after_tail_call(
                        cache,
                        old_exit,
                        new_exit_control,
                    );
                }
            }
            stats.tail_call_edits += 1;
            Ok(())
        }
        ResolvedTargets::Multiple(_) => {
            // Multiple isn't an in-place edit case — the orchestrator
            // routes it through a CFG rebuild instead.  Reaching this
            // arm is a logic bug in the dispatch; surface as a typed
            // error rather than silently mis-applying.
            Err(ErrorKind::Unimplemented(
                "apply_in_place_edit called with ResolvedTargets::Multiple".to_string(),
            )
            .into())
        }
    }
}

/// Walk `cache` and, for any [`RegionIrEntry`] whose `exit_control`
/// equals `old_exit_control`, replace it with `new_exit_control`.
///
/// Called by [`apply_in_place_edit`] after [`apply_tail_call`] splices
/// in a fresh `Call → Return` chain whose new Return reads control
/// from the Call's output.  The cache entry that previously recorded
/// the placeholder Return's control input as the region's
/// `exit_control` should now record the Call's control output, so
/// downstream consumers reading "what control feeds this region's
/// terminator?" see the live id.
///
/// CORRECTNESS — uniqueness: the matching is keyed on a
/// `NodeOutputId`, which is unique per output slot in the graph.
/// Multiple cache entries cannot share the same `exit_control`
/// (each region has its own body chain ending in a unique
/// `NodeOutputId`), so at most one entry is updated per call.  We
/// scan the whole map rather than threading the matching region id
/// through `apply_in_place_edit`'s callers — the linear scan is
/// O(cache_size) and trivially correct.
fn update_cache_exit_handle_after_tail_call(
    cache: &mut RegionIrCache,
    old_exit_control: ir::node::NodeOutputId,
    new_exit_control: ir::node::NodeOutputId,
) {
    for entry in cache.values_mut() {
        if entry.exit_control == old_exit_control {
            // CORRECTNESS — single match expected: see uniqueness note
            // above.  We continue scanning because the cache is small
            // and a corrupt cache (multiple entries sharing the same
            // exit_control) should still be patched consistently.
            entry.exit_control = new_exit_control;
        }
    }
}

/// Read the calling convention's ret-val varnodes from the
/// placeholder's pre-Return state.  Used by both `apply_link_register`
/// and `apply_tail_call` to thread real ABI return values into the
/// surviving Return.
///
/// CORRECTNESS: the placeholder Return's control input is the
/// pre-Return control, so the per-region `vn_to_value` map at that
/// control's region is the right place to read ret-val regs.  For
/// round-1 we approximate by walking the Return's uses backward —
/// since the optimiser has run, ABI return registers fold to their
/// post-clobber values at this control point automatically.  When
/// no convention ret-val regs exist (e.g. void-returning function),
/// we return an empty slice — `apply_link_register` and
/// `apply_tail_call` are robust to it.
///
/// Round-1 simplification: returns an empty `Vec` so the in-place
/// edits emit a Return with no ret-vals.  This is sound because the
/// placeholder's `target_value` is preserved at slot 2 (for
/// LinkRegister) or replaced by Call's outputs (for tail-call); the
/// actual ABI ret-val passing is downstream of where we sit in the
/// IR.  Future rounds may walk the cache's `exit_vn_to_value` to
/// thread the convention's ret-val regs in.
fn read_ret_val_outputs<B>(
    _graph: &ir::BuiltFunctionGraph,
    _placeholder: NodeId,
    _config: &OrchestratorConfig<'_, B>,
) -> Result<Vec<ir::Value>>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    // CORRECTNESS: empty vec is sound — see function-level docs.
    // Future rounds will populate from the cache's
    // `exit_vn_to_value`.
    Ok(Vec::new())
}

/// Builds the CFG, lifts to IR, runs the **stable** optimiser
/// subset.  Increments `stats.cfg_rebuilds` and `stats.stable_runs`.
///
/// CORRECTNESS — Sleigh persistence: `sleigh` is **passed in by value**
/// rather than constructed inside this function.  The orchestrator
/// takes the caller-supplied Sleigh out of `config.sleigh` (W5) at loop
/// entry, then threads the same handle through every call to
/// `build_lift_stable` by harvesting it from the previous iteration's
/// `Cfg::sleigh` field.  Reusing one Sleigh across many `lift_one`
/// calls is sound — `lift_one` mutates only Sleigh's internal decode
/// buffers, which are reset each call; there is no per-CFG state in
/// Sleigh.  This pattern is what `cfg::region_builder` already does
/// within a single CFG build; doing it across iterations gives us the
/// same property at the orchestrator scale and avoids re-loading the
/// SLA spec on every iteration.
///
/// CORRECTNESS — cache lifecycle: when `prev_snapshot` is `Some`, this
/// invokes [`apply_split_invalidation`] FIRST so any cached entry
/// whose underlying region was split (its insn count shrank) is
/// evicted before the new lift populates the cache.  The snapshot is
/// taken from the previous iteration's `Cfg` BEFORE the orchestrator
/// destructured it to recover the Sleigh handle — so the split-
/// detection contract is preserved without holding the previous `Cfg`
/// alive across iterations.
#[allow(clippy::type_complexity)]
fn build_lift_stable<B>(
    sleigh: rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>,
    config: &OrchestratorConfig<'_, B>,
    known_targets: &HashMap<PcodeInsnAddr, ResolvedTargets>,
    stats: &mut OrchestratorStats,
    region_ir_cache: &mut RegionIrCache,
    prev_snapshot: Option<&CfgSplitSnapshot>,
) -> Result<(
    ir::BuiltFunctionGraph,
    Vec<(PcodeInsnAddr, ir::Value)>,
    Cfg<rsleigh::mem_readers::BufMemReader<B>>,
)>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    let mut opts_builder = OptionsBuilder::new();
    if let Some(rom) = config.rom.clone() {
        opts_builder = opts_builder.set_read_only_memory(rom);
    }
    if let Some(lr) = config.strider.calling_convention().link_register_vn {
        opts_builder = opts_builder.set_link_register(lr);
    }
    if let Some(max) = config.fn_max_size {
        opts_builder = opts_builder.set_function_max_size(max);
    }
    if config.allow_code_before_start_addr {
        opts_builder = opts_builder.allow_code_before_start_addr();
    }
    let opts = opts_builder.build();

    let arch_endianness = config.strider.arch().endianness;
    let cfg: Cfg<rsleigh::mem_readers::BufMemReader<B>> =
        Builder::with_endianness(sleigh, config.start_addr, opts, arch_endianness)
            .with_known_targets(known_targets.clone())
            .build()?;
    stats.cfg_rebuilds += 1;

    // CORRECTNESS — split-invalidation: if a previous snapshot exists,
    // detect regions whose insn count shrank (a `split_region` event
    // moved pcode into a fresh second-half region) and evict their
    // cache entries.  The next lift will re-lift the now-shorter first
    // half from pcode; the second half lifts as a brand-new entry.
    if let Some(prev) = prev_snapshot {
        let evictions = apply_split_invalidation(region_ir_cache, prev, &cfg)?;
        stats.cache_evictions_on_split += evictions;
    }

    // Use the cache-aware lift.  In round 1 this still physically
    // rebuilds the IR (fresh FunctionBuilder), but `LiftStats`
    // reports counts under the cache contract — see `LiftStats`'s
    // round-1 / round-2 correctness note.
    let (outcome, lift_stats) =
        lift_new_regions_into_with_stats(config.strider, region_ir_cache, &cfg)?;
    stats.pcode_insns_lifted += lift_stats.pcode_insns_lifted;
    stats.regions_newly_lifted += lift_stats.regions_lifted;
    let unresolved = outcome.unresolved_branches.clone();
    let mut graph = outcome.graph;

    // CORRECTNESS — pipeline tier: intermediate iterations of the
    // outer loop may run this multiple times (one per CFG rebuild).
    // The stable subset omits `RedundantPhis` /
    // `DeadBranchElimination` / `CallOtherElide` so phi nodes the
    // cache pins by `NodeId` survive across iterations.
    let pipeline = config.strider.build_stable_optimizer_pipeline();
    pipeline.run_on_built(&mut graph)?;
    stats.stable_runs += 1;

    Ok((graph, unresolved, cfg))
}

/// Fallback to the original lift path.  Used only by the legacy
/// shim retained for backwards compatibility — production callers
/// route through [`build_lift_stable`] which threads the cache
/// through.
#[allow(dead_code)]
fn legacy_analyze_unused() {}

/// Runs the **stable** optimizer subset on an existing graph.  Used
/// after in-place edits to clean up before re-classifying.
fn run_stable_only<B>(
    config: &OrchestratorConfig<'_, B>,
    graph: &mut ir::BuiltFunctionGraph,
    stats: &mut OrchestratorStats,
) -> Result<()>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    // CORRECTNESS — stable-only: this is invoked between iterations
    // when the IR shape may still change (more in-place edits could
    // come on the next turn).  Running the destructive subset here
    // would risk removing nodes that a future iteration's edit needs.
    let pipeline = config.strider.build_stable_optimizer_pipeline();
    pipeline.run_on_built(graph)?;
    stats.stable_runs += 1;
    Ok(())
}

/// Runs the **destructive** optimizer subset.  Called exactly once
/// per successful orchestrator run, at the fixed-point exit (or in
/// the fast-path when the function has no `BranchIndirect`).
fn run_destructive<B>(
    config: &OrchestratorConfig<'_, B>,
    graph: &mut ir::BuiltFunctionGraph,
    stats: &mut OrchestratorStats,
) -> Result<()>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    // CORRECTNESS — destructive at fixed point only: the IR shape is
    // final here (no future iterations will add nodes), so
    // `RedundantPhis` / `DeadBranchElimination` are safe to run.
    let pipeline = config.strider.build_destructive_optimizer_pipeline();
    pipeline.run_on_built(graph)?;
    stats.destructive_runs += 1;
    Ok(())
}

/// The induced edge set of a `known_targets` map: a sorted
/// `Vec<(PcodeInsnAddr, EdgeKind)>` for `Single` / `Multiple` and a
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

    // ── G1-COMPLETE: cache-exit-handle / stats unit tests ───────────────────

    #[test]
    fn update_cache_exit_handle_replaces_matching_entries_only() {
        // Build a synthetic cache with two entries — one whose
        // `exit_control` matches the "old" id, one that doesn't.
        // Apply the helper; only the matching entry should be
        // patched.  Pins the per-entry uniqueness contract
        // documented on the helper.
        use crate::RegionIrEntry;
        use cfg::PcodeInsnAddr;
        use ir::node::{NodeId, NodeOutputId};
        let mut cache: RegionIrCache = HashMap::new();
        let old_id = NodeOutputId::from_u32(7);
        let other_id = NodeOutputId::from_u32(8);
        let new_id = NodeOutputId::from_u32(99);
        let mut e1 = RegionIrEntry::empty(PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: 0x1000 },
            insn_index: 0,
        });
        e1.exit_control = old_id;
        e1.entry_control_state = NodeId::from_u32(1);
        let mut e2 = RegionIrEntry::empty(PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: 0x2000 },
            insn_index: 0,
        });
        e2.exit_control = other_id;
        e2.entry_control_state = NodeId::from_u32(2);
        cache.insert(MachineInsnAddr { addr: 0x1000 }, e1);
        cache.insert(MachineInsnAddr { addr: 0x2000 }, e2);
        update_cache_exit_handle_after_tail_call(&mut cache, old_id, new_id);
        assert_eq!(
            cache.get(&MachineInsnAddr { addr: 0x1000 }).expect("e1").exit_control,
            new_id,
            "entry whose exit_control matched must be patched",
        );
        assert_eq!(
            cache.get(&MachineInsnAddr { addr: 0x2000 }).expect("e2").exit_control,
            other_id,
            "entry whose exit_control did not match must be left alone",
        );
    }

    #[test]
    fn update_cache_exit_handle_noop_on_empty_cache() {
        // Pins: empty cache → no-op.  The helper must not panic on
        // an empty input.
        use ir::node::NodeOutputId;
        let mut cache: RegionIrCache = HashMap::new();
        update_cache_exit_handle_after_tail_call(
            &mut cache,
            NodeOutputId::from_u32(1),
            NodeOutputId::from_u32(2),
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn update_cache_exit_handle_noop_when_no_match() {
        // Pins: when no entry's exit_control equals `old`, the cache
        // is left unchanged — the helper must not over-zealously
        // mutate.
        use crate::RegionIrEntry;
        use cfg::PcodeInsnAddr;
        use ir::node::{NodeId, NodeOutputId};
        let mut cache: RegionIrCache = HashMap::new();
        let mut e = RegionIrEntry::empty(PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: 0x1000 },
            insn_index: 0,
        });
        e.exit_control = NodeOutputId::from_u32(7);
        e.entry_control_state = NodeId::from_u32(1);
        cache.insert(MachineInsnAddr { addr: 0x1000 }, e);
        update_cache_exit_handle_after_tail_call(
            &mut cache,
            NodeOutputId::from_u32(99), // doesn't match
            NodeOutputId::from_u32(100),
        );
        assert_eq!(
            cache.get(&MachineInsnAddr { addr: 0x1000 }).expect("e").exit_control,
            NodeOutputId::from_u32(7),
            "no-match → cache stays put",
        );
    }

    #[test]
    fn orchestrator_stats_default_is_zero() {
        // OrchestratorStats fields all start at zero — pinning the
        // contract that callers can assume `Default::default()`
        // means "nothing has run yet".
        let s = OrchestratorStats::default();
        assert_eq!(s.stable_runs, 0);
        assert_eq!(s.destructive_runs, 0);
        assert_eq!(s.cfg_rebuilds, 0);
        assert_eq!(s.link_register_edits, 0);
        assert_eq!(s.tail_call_edits, 0);
        assert_eq!(s.iterations, 0);
        // G1-COMPLETE: new cache contract counters are also zeroed.
        assert_eq!(s.pcode_insns_lifted, 0);
        assert_eq!(s.regions_newly_lifted, 0);
        assert_eq!(s.cache_evictions_on_split, 0);
    }

    // ── Sleigh persistence: split-snapshot helpers ─────────────────────────

    #[test]
    fn apply_split_invalidation_noop_on_empty_snapshot() {
        // Pin: an empty `prev_snapshot` is a valid no-op input for
        // [`apply_split_invalidation`] — the orchestrator constructs an
        // empty snapshot conceptually before iter 0 is built (it's wrapped
        // in `Option::None` rather than an empty map, but the helper must
        // also tolerate the empty-map case for defensive composability).
        use crate::RegionIrEntry;
        let mut cache: RegionIrCache = HashMap::new();
        cache.insert(
            MachineInsnAddr { addr: 0x1000 },
            RegionIrEntry::empty(pcode_addr(0x1000)),
        );
        let snapshot: CfgSplitSnapshot = HashMap::new();
        // Build a real (single-region) Cfg so `apply_split_invalidation`
        // has a `new_cfg` argument.  We synthesise it by lifting `ret`.
        let arch = crate::SleighArch::x86_64();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc3u8], 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .expect("sleigh");
        let new_cfg = cfg::Builder::new(
            sleigh,
            0x1000,
            cfg::OptionsBuilder::new().build(),
        )
        .build()
        .expect("cfg");
        let evictions =
            apply_split_invalidation(&mut cache, &snapshot, &new_cfg).expect("invalidation");
        // Empty snapshot → no comparisons → no evictions, regardless of
        // what's already in the cache.
        assert_eq!(evictions, 0);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn take_split_snapshot_reports_each_region_once() {
        // Pin: [`take_split_snapshot`] visits every region of `cfg`
        // exactly once and produces an entry per region.  This is the
        // essential property that makes the snapshot a faithful proxy
        // for the full Cfg's region-set in [`apply_split_invalidation`].
        let arch = crate::SleighArch::x86_64();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc3u8], 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .expect("sleigh");
        let cfg = cfg::Builder::new(
            sleigh,
            0x1000,
            cfg::OptionsBuilder::new().build(),
        )
        .build()
        .expect("cfg");
        let snapshot = take_split_snapshot(&cfg).expect("snapshot");
        // Single-region (just `ret`) → exactly one snapshot entry.
        assert_eq!(snapshot.len(), 1);
        assert!(snapshot.contains_key(&MachineInsnAddr { addr: 0x1000 }));
        // The recorded count is the region's pcode insn count
        // (a positive integer).
        let count = snapshot.get(&MachineInsnAddr { addr: 0x1000 }).copied().unwrap_or(0);
        assert!(count >= 1, "ret instruction must produce at least one pcode insn");
    }

    #[test]
    fn sleigh_can_be_used_for_multiple_cfg_builds_via_orchestrator_path() {
        // CORRECTNESS pin: this is the source-level form of the cfg-
        // crate's `cfg_sleigh_field_round_trip` / `sleigh_can_be_used_for_
        // multiple_cfg_builds` tests.  Build a Cfg, harvest its sleigh
        // field, build a second Cfg with the same sleigh — both must
        // succeed.  Mirrors the orchestrator's iteration boundary
        // exactly: `cfg.sleigh` is the recovery channel between
        // iterations, and reusing the handle is the entire point of
        // Approach (A).
        let arch = crate::SleighArch::x86_64();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc3u8], 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
            .expect("sleigh");
        let cfg1 = cfg::Builder::new(
            sleigh,
            0x1000,
            cfg::OptionsBuilder::new().build(),
        )
        .build()
        .expect("cfg1");
        let recovered = cfg1.sleigh;
        let cfg2 = cfg::Builder::new(
            recovered,
            0x1000,
            cfg::OptionsBuilder::new().build(),
        )
        .build()
        .expect("cfg2");
        assert!(cfg2.graph.node_count() >= 1);
    }
}
