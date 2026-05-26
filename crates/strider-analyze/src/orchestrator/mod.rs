//! Top-level analysis driver.
//!
//! [`run`] is the canonical entry point: build the CFG, lift to IR,
//! run the optimiser pipeline, resolve indirect branches via the
//! indirect-resolution fixed-point loop, and return the final IR graph.
//!
//! ## Iteration shape
//!
//! 1. Build the CFG with the current `known_targets` map.
//! 2. Lift the CFG to IR via [`Strider::analyze_cfg`].
//! 3. Run the **stable** optimiser subset
//!    ([`Strider::build_stable_optimizer_pipeline`]).
//! 4. For each unresolved anchor, run
//!    [`crate::opt::indirect_branch_resolve::classify_anchor`].
//! 5. Apply in-place IR edits for terminal classifications:
//!    [`crate::opt::indirect_branch_resolve::apply_link_register`] for `LinkRegister`,
//!    [`crate::opt::indirect_branch_resolve::apply_tail_call`] for `Single(K)` where `K` is outside
//!    the function range.  These do NOT trigger a CFG rebuild.
//! 6. If any classification requires a structural rebuild (intra-fn
//!    `Single`, `Multiple` jump table), update `known_targets` and
//!    rebuild the CFG.  Otherwise stay on the same CFG.
//! 7. At fixed point: if any branch is still unresolved, return
//!    `Err`.  Otherwise run the destructive subset
//!    ([`Strider::build_destructive_optimizer_pipeline`]) once and
//!    return the optimised IR.
//!
//! ## Iteration cap
//!
//! The cap `2 * pending_at_iter_0 + 4` is a conservative bound on the
//! number of legal classification transitions: every transition
//! strictly grows the induced edge set.  Plus a separate stall budget
//! tracks consecutive in-place-only iterations (which don't grow the
//! edge set) so the loop cannot spin indefinitely on an
//! IR-level indirect-branch resolver soundness bug.
//!
//! ## Tail-call detection
//!
//! A `Single(K)` resolution where `K` lies outside the function
//! address range is treated as a tail call and applied as an in-place
//! edit.  Inside-the-function `Single(K)` requires a CFG rebuild
//! because new code becomes reachable.

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{anyhow, bail, Result};

use strider_lift::cfg::{Builder, Cfg, DecodeCache, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::{NodeId, NodeOutputId};
use crate::opt::ReadOnlyMemory;

use crate::opt::indirect_branch_resolve::{
    apply_link_register, apply_tail_call, classify_anchor,
};
use crate::pattern::GraphRewriteCtxExt;
use crate::strider::{RegionLiftHandles, Strider};

/// Configuration for [`run`].  Held outside the function so callers
/// can construct one and reuse the strider / sleigh / options across
/// iterations without re-paying per-iteration setup costs.
pub struct Config<'a, R>
where
    R: rsleigh::MemReader,
{
    /// The strider — stable across iterations.
    pub strider: &'a Strider,
    /// Function entry address.  Newtype prevents accidental swap with
    /// `fn_max_size` at struct-literal construction sites.  Construct
    /// via `addr.into()` or `strider_lift::cfg::MachineInsnAddr::from(addr)`.
    pub start_addr: strider_lift::cfg::MachineInsnAddr,
    /// The Sleigh context, owned and threaded through every iteration
    /// of the fixed-point loop.  Re-using one Sleigh across iterations
    /// avoids re-loading the SLA spec on every CFG rebuild.
    pub sleigh: rsleigh::Sleigh<R>,
    /// Read-only memory image for the optimiser's `LoadReadOnly`
    /// pass.  `None` to disable.  Borrowed via `as_deref()` for the
    /// classifier path; `Arc::clone`d once per CFG rebuild for the
    /// cfg builder's option.
    pub rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
    /// Maximum function size in bytes.  When set, a `Single(K)`
    /// resolution with `K >= start_addr + fn_max_size` is treated as a
    /// tail call.  When `None`, only `K < start_addr` is treated as a
    /// tail call.
    pub fn_max_size: Option<u64>,
    /// When `true`, `Single(K)` with `K < start_addr` is NOT treated
    /// as a tail call — i.e. the orchestrator follows it as an
    /// intra-fn branch.
    pub allow_code_before_start_addr: bool,
    /// Compact the IR arena at finalize, dropping nodes that aren't
    /// reachable from `entry` via [`strider_ir::graph::Graph::walk_from`].  Default
    /// `true` is recommended (passes leave detached "zombie" nodes
    /// the destructive pipeline severs from the live graph; without
    /// compaction these stay in the arena).  Pre-compaction NodeIds
    /// become invalid across the call.
    pub compact: bool,
    /// Per-target-address calling-convention overrides.  When a `Call`
    /// is emitted (either at lift time for a direct call to an
    /// `IntConst(K)` target, or by the indirect-branch resolver as an
    /// in-place tail-call edit to address `K`), if `K` is in this map
    /// the matching CC fully replaces the function-default for that
    /// one Call.  Empty by default.
    ///
    /// Driver: Linux-kernel `__fentry__` / `mcount` hooks that preserve
    /// every register and observe no arguments — express via
    /// [`strider_target::CallingConvention::x86_64_all_preserving`] (and the
    /// per-arch siblings).  The user supplies raw addresses; symbol
    /// resolution is the caller's responsibility.
    pub per_address_ccs: FxHashMap<u64, strider_target::CallingConvention>,
}

/// Per-iteration index built from a lift's [`RegionLiftHandles`]
/// snapshot.  Maps a region's exit-control `NodeOutputId` to the
/// region's exit `vn_to_value` table — what
/// [`crate::opt::AnchorCallingContext::for_anchor`] needs to thread
/// ABI varnodes through an in-place edit.
///
/// Owned by value (each `RegionLiftHandles` is consumed once, by
/// `from_handles`, via `into_iter`).  Keyed by `NodeOutputId` which
/// impls `EntityRef`, so `FxHashMap` (not `std::HashMap`'s SipHash) is
/// the appropriate entity-keyed map per CLAUDE.md.
type ExitVnToValue = rustc_hash::FxHashMap<rsleigh::Vn, NodeOutputId>;

struct RegionIndex {
    by_exit_control: rustc_hash::FxHashMap<NodeOutputId, ExitVnToValue>,
}

impl RegionIndex {
    fn from_handles(handles: Vec<RegionLiftHandles>) -> Self {
        let mut by_exit_control =
            rustc_hash::FxHashMap::with_capacity_and_hasher(handles.len(), Default::default());
        for h in handles.into_iter() {
            by_exit_control.insert(h.exit_control, h.exit_vn_to_value);
        }
        Self { by_exit_control }
    }

    fn region_for_placeholder(
        &self,
        graph: &strider_ir::Graph,
        placeholder: NodeId,
    ) -> Option<&ExitVnToValue> {
        let ctrl_in = graph.nth_input(placeholder, 0)?;
        self.by_exit_control.get(&ctrl_in)
    }
}

/// Drives the iterate-resolve-feed-back loop.
///
/// # Errors
///
/// Returns an error when the iteration cap is hit, when unresolved
/// branches remain at fixed point, or any error propagated from
/// strider / cfg / opt.
pub fn run<R>(config: Config<'_, R>) -> Result<strider_ir::Function>
where
    R: rsleigh::MemReader,
{
    let mut state = LoopState::new(config)?;
    state.build_initial_iteration()?;
    if state.no_unresolved() {
        return state.finalize();
    }
    let cap = state.cap();
    for _ in 0..cap {
        match state.step()? {
            Decision::FixedPoint => return state.finalize(),
            Decision::StableOnly => state.run_stable_only()?,
            Decision::Rebuild => state.rebuild()?,
        }
    }
    bail!("indirect-branch resolver did not converge after {cap} iterations")
}

/// Outcome of one [`LoopState::step`] call.
enum Decision {
    /// Edge set didn't change AND no in-place edits fired.  Run the
    /// destructive subset and return.
    FixedPoint,
    /// In-place edits fired but the induced edge set didn't change.
    /// Re-run the stable subset on the freshly-edited IR; loop.
    StableOnly,
    /// Edge set changed.  Rebuild the CFG with updated
    /// `known_targets`; loop.
    Rebuild,
}

/// Stall-guard helper for the fixed-point loop's `step` method.
/// extracted to a free function so the
/// invariant can be unit-tested directly without constructing a
/// real `LoopState` (which requires a `Sleigh<R>`, `Graph`,
/// and full CFG state).
///
/// Fires `Err` when an in-place-only iteration's unresolved count
/// **strictly grew** AND the budget is exhausted.  Count-stable
/// iterations (`unresolved_after == prev_unresolved`) do NOT
/// consume budget: they may represent real progress through an
/// anchor-replacement chain (one anchor resolved, one new
/// placeholder materialised).  The outer
/// `cap = 2 * pending_at_iter_0 + 4` bound still terminates
/// count-stable infinite loops.
///
/// Pre-fix the comparison was `>=`, which
/// incorrectly consumed budget on every count-stable iteration.
///
/// # Errors
///
/// Returns `Err` when `!edge_set_changed && unresolved_after >
/// unresolved_before && *stall_budget == 0`.  Otherwise decrements
/// `stall_budget` (when both growth and no-edge-change conditions
/// hold) and returns `Ok(())`.
fn apply_stall_guard(
    stall_budget: &mut usize,
    edge_set_changed: bool,
    unresolved_after: usize,
    unresolved_before: usize,
) -> Result<()> {
    if !edge_set_changed && unresolved_after > unresolved_before {
        if *stall_budget == 0 {
            bail!(
                "in-place edits stalled: {} unresolved branches after edit (grew from {}), no edge-set growth",
                unresolved_after,
                unresolved_before,
            );
        }
        *stall_budget -= 1;
    }
    Ok(())
}

/// The fixed-point loop's spanning state.
struct LoopState<'a, R>
where
    R: rsleigh::MemReader,
{
    /// The strider — stable across iterations.
    strider: &'a Strider,
    /// Function entry address; copied from [`Config::start_addr`].
    start_addr: strider_lift::cfg::MachineInsnAddr,
    /// Read-only memory image; copied from [`Config::rom`].
    rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
    /// Function-size cap; copied from [`Config::fn_max_size`].
    fn_max_size: Option<u64>,
    /// Code-before-start permission; copied from [`Config::allow_code_before_start_addr`].
    allow_code_before_start_addr: bool,
    /// Compaction flag for the finalize step; copied from [`Config::compact`].
    compact: bool,
    /// Pre-resolved per-target-address CC overrides.  See the
    /// [`Config::per_address_ccs`] doc.  Resolved once at
    /// `LoopState::new` so any unresolved register name surfaces
    /// before iteration starts.
    per_address_built_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// Accumulator of IR-level indirect-branch resolver resolutions across iterations.
    /// Monotonically grows: once an anchor's targets land here, the
    /// CFG-rebuild path keeps using them.  Per-iteration classifications
    /// overlay this map (so an upgrade like
    /// `Single(K1) → Multiple([K1, K2])` overwrites the entry), but
    /// anchors that are no longer in the per-iteration `unresolved`
    /// list (because the previous Rebuild lowered them to switch
    /// edges) MUST stay — wiping them re-introduces the placeholder
    /// on the next rebuild and the loop diverges.
    known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    /// The Sleigh handle we thread through every iteration.  Initialised
    /// from `Config::sleigh` at construction; consumed by
    /// `Builder::for_arch` per iteration and harvested back from the
    /// resulting `Cfg::into_sleigh()`.  `None` only momentarily inside
    /// `build_lift_stable`.
    sleigh: Option<rsleigh::Sleigh<R>>,
    /// The current optimised IR function.  Initialised to an empty
    /// placeholder by [`LoopState::new`] and overwritten with the real
    /// lift result by [`LoopState::build_initial_iteration`] before any
    /// consumer reads it; the empty placeholder is never observed past
    /// construction.  No `Option` wrapper because the post-init
    /// invariant is "always populated" — paying `as_ref().ok_or_else`
    /// on every read for an unreachable `None` branch is pure cost.
    function: strider_ir::Function,
    /// Pending placeholder anchors for the current iteration.
    unresolved: Vec<(PcodeInsnAddr, strider_ir::Value)>,
    /// Pending count at iter 0; sets the cap.
    pending_at_iter_0: usize,
    /// Remaining stall budget.  Decrements each consecutive
    /// in-place-only iteration that didn't reduce `unresolved`; reaching
    /// zero is a misclassifying-resolver bug, surfaced as a typed error.
    stall_budget: usize,
    /// Per-iteration region index, rebuilt by `build_initial_iteration` /
    /// `rebuild` from the latest `RegionLiftHandles` snapshot.
    region_index: RegionIndex,
    /// Cached link-register / stack-pointer varnodes (stable across
    /// iterations).
    lr_vn: Option<rsleigh::Vn>,
    stack_vn: Option<rsleigh::Vn>,
    /// Decode cache shared across CFG rebuilds.  The Sleigh handle
    /// persists for the whole `run`, so this cache stays valid for
    /// every iteration; threaded into each fresh `strider_lift::cfg::Builder` so
    /// machine-instruction decodes are paid once per address per run.
    decode_cache: DecodeCache,
    /// Cached set of varnodes seen so far across all CFG iterations.
    /// Amortises `find_all_unique_vns` across CFG-rebuild iterations:
    /// without the cache, every Rebuild iteration would re-scan every
    /// region's every instruction's every varnode; here we only scan
    /// the regions added since the previous iteration (petgraph's
    /// `StableDiGraph` allocates monotonic `NodeIndex`s, so the new
    /// regions sit at indices `[prev_count..current_count)`).
    ///
    /// The set is conservative under region splits: a split region's
    /// original vns stay in the cache even if the original's insn
    /// list got truncated.  Over-tracking a vn allocates one extra
    /// `InitialVar` (cheap) and never miscompiles.
    vn_cache: rustc_hash::FxHashSet<rsleigh::Vn>,
    /// Region count at the most recent `find_all_unique_vns` call.
    /// `vn_cache` is up-to-date for the first `vn_cache_region_count`
    /// regions in the CFG; later regions need to be scanned and unioned
    /// into the cache.
    vn_cache_region_count: usize,
}

impl<'a, R> LoopState<'a, R>
where
    R: rsleigh::MemReader,
{
    fn new(config: Config<'a, R>) -> Result<Self> {
        let lr_vn = config.strider.calling_convention().link_register_vn;
        let stack_vn = Some(config.strider.calling_convention().stack_vn);
        // Pre-resolve per-address CC overrides against the same Sleigh
        // register table the function-default CC was built against.
        let per_address_built_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention> =
            if config.per_address_ccs.is_empty() {
                FxHashMap::default()
            } else {
                let sleigh_regs = config
                    .sleigh
                    .regs()
                    .map_err(|e| anyhow!("orchestrator: Sleigh::regs() failed: {e:?}"))?;
                config
                    .per_address_ccs
                    .iter()
                    .map(|(addr, cc)| {
                        (*cc)
                            .build(&sleigh_regs)
                            .map(|built| (*addr, built))
                            .map_err(|e| {
                                anyhow!("per-address CC at {addr:#x} unresolved: {e:?}")
                            })
                    })
                    .collect::<Result<_>>()?
            };
        Ok(Self {
            sleigh: Some(config.sleigh),
            known_targets: FxHashMap::default(),
            // Empty placeholder; overwritten by `build_initial_iteration`
            // before any consumer reads it.
            function: strider_ir::Function::new(),
            unresolved: Vec::new(),
            pending_at_iter_0: 0,
            stall_budget: 0,
            region_index: RegionIndex {
                by_exit_control: rustc_hash::FxHashMap::default(),
            },
            lr_vn,
            stack_vn,
            decode_cache: DecodeCache::new(),
            vn_cache: rustc_hash::FxHashSet::default(),
            vn_cache_region_count: 0,
            strider: config.strider,
            start_addr: config.start_addr,
            rom: config.rom,
            fn_max_size: config.fn_max_size,
            allow_code_before_start_addr: config.allow_code_before_start_addr,
            compact: config.compact,
            per_address_built_ccs,
        })
    }

    /// Iteration 0: build the CFG, lift, run stable opt, snapshot the
    /// region index.
    fn build_initial_iteration(&mut self) -> Result<()> {
        self.lift_and_seat("build_initial_iteration")?;
        self.pending_at_iter_0 = self.unresolved.len();
        // Allow an in-place-only stall for at most `pending_at_iter_0`
        // iterations: each in-place edit must remove at least one
        // placeholder, so we can't legitimately stall that many times
        // in a row without making progress.
        self.stall_budget = self.pending_at_iter_0;
        Ok(())
    }

    /// Drive `build_lift_stable` once and seat the resulting graph,
    /// region index, and unresolved-branch list onto `self`.  Shared
    /// helper between [`Self::build_initial_iteration`] (initial lift) and
    /// [`Self::rebuild`] (post-Rebuild re-lift).  `phase` names the
    /// caller for the error message when the Sleigh handle is
    /// missing.
    fn lift_and_seat(&mut self, phase: &'static str) -> Result<()> {
        let sleigh = self
            .sleigh
            .take()
            .ok_or_else(|| anyhow!("orchestrator: sleigh handle missing at {phase}"))?;
        let (function, unresolved, region_index, sleigh) = self.build_lift_stable(sleigh)?;
        self.sleigh = Some(sleigh);
        self.region_index = region_index;
        self.function = function;
        self.unresolved = unresolved;
        Ok(())
    }

    /// Build the CFG, lift to IR, run the stable optimiser subset.
    /// Returns `(graph, unresolved, region_index, sleigh)` so the caller
    /// can re-use the harvested Sleigh handle across iterations.
    ///
    /// Sequencer: delegates CFG construction to [`build_cfg`], runs the
    /// IR lift via [`Strider::analyze_cfg_with`], harvests the post-lift
    /// varnode delta via [`scan_new_vns`], and finishes with the stable
    /// optimiser pipeline.  The named helpers carry the per-step
    /// commentary.
    #[allow(clippy::type_complexity)]
    fn build_lift_stable(
        &mut self,
        sleigh: rsleigh::Sleigh<R>,
    ) -> Result<(
        strider_ir::Function,
        Vec<(PcodeInsnAddr, strider_ir::Value)>,
        RegionIndex,
        rsleigh::Sleigh<R>,
    )> {
        let cfg = build_cfg(
            sleigh,
            self.strider,
            self.start_addr,
            self.rom.clone(),
            self.fn_max_size,
            self.allow_code_before_start_addr,
            &self.known_targets,
            &self.decode_cache,
        )?;

        let all_vns = scan_new_vns(&cfg, &mut self.vn_cache, &mut self.vn_cache_region_count);

        let outcome = self.strider.analyze_cfg_with(
            &cfg,
            crate::AnalyzeOptions {
                all_vns: Some(all_vns),
                per_address_ccs: Some(&self.per_address_built_ccs),
            },
        )?;
        let region_index = RegionIndex::from_handles(outcome.region_handles);
        let mut function = outcome.function;
        let unresolved = outcome.unresolved_branches;

        let pipeline = self.strider.build_stable_optimizer_pipeline();
        let entry = function.entry().ok_or_else(|| {
            anyhow::anyhow!("seat: entry node is not set")
        })?;
        pipeline.run(&mut function, entry)?;

        // Harvest the Sleigh handle out of the consumed Cfg so the next
        // iteration can re-use it without re-loading the SLA spec.
        let harvested = cfg.into_sleigh();
        Ok((function, unresolved, region_index, harvested))
    }

    fn no_unresolved(&self) -> bool {
        self.unresolved.is_empty()
    }

    fn cap(&self) -> usize {
        2usize
            .saturating_mul(self.pending_at_iter_0)
            .saturating_add(4)
    }

    /// Run one iteration of the loop.
    fn step(&mut self) -> Result<Decision> {
        // Snapshot `prev_unresolved_len` BEFORE `apply_in_place_edits` so the
        // stall-guard's "before" count is the count entering this `step`.
        // Today `apply_in_place_edits` doesn't mutate `self.unresolved` (it
        // only mutates the graph), so reading after would be accidentally
        // correct — but a future change that prunes the list during edits
        // would silently break the stall-guard baseline.  Capture early so
        // the data dependency is explicit.
        let prev_unresolved_len = self.unresolved.len();
        let (next_known, in_place_edits) = self.classify_and_partition()?;
        self.apply_in_place_edits(&in_place_edits)?;
        let unresolved_after_edits = self.recompute_unresolved(&in_place_edits)?;

        let edge_set_changed = edge_set_of(&next_known) != edge_set_of(&self.known_targets);
        if !edge_set_changed && in_place_edits.is_empty() {
            // Fixed point.  Any branch in `unresolved_after_edits`
            // not in `next_known` is genuinely unresolvable.
            self.unresolved = unresolved_after_edits;
            if let Some(addr) = self.unresolved.iter().find_map(|(addr, _)| {
                if next_known.contains_key(addr) {
                    None
                } else {
                    Some(*addr)
                }
            }) {
                return Err(anyhow!("indirect branch at {addr:?} could not be resolved at fixed point"));
            }
            return Ok(Decision::FixedPoint);
        }

        // Track stall guard via the apply_stall_guard helper.
        apply_stall_guard(
            &mut self.stall_budget,
            edge_set_changed,
            unresolved_after_edits.len(),
            prev_unresolved_len,
        )?;

        self.unresolved = unresolved_after_edits;
        if !edge_set_changed {
            return Ok(Decision::StableOnly);
        }
        self.known_targets = next_known;
        Ok(Decision::Rebuild)
    }

    /// Re-run the stable subset on the current graph (after in-place
    /// edits).  Used when the loop chose [`Decision::StableOnly`].
    fn run_stable_only(&mut self) -> Result<()> {
        let pipeline = self.strider.build_stable_optimizer_pipeline();
        let entry = self.function.entry().ok_or_else(|| {
            anyhow::anyhow!("run_stable_only: entry node is not set")
        })?;
        pipeline.run(&mut self.function, entry)?;
        Ok(())
    }

    /// Rebuild the CFG with the updated `known_targets` map and
    /// re-lift.  Used when the loop chose [`Decision::Rebuild`].
    ///
    /// Also resets `stall_budget` based on the *post-rebuild*
    /// unresolved count.  The budget tracks consecutive in-place-only
    /// iterations that fail to make progress; a Rebuild is by
    /// definition forward progress (the edge set just grew), so the
    /// stall counter should restart from a budget proportional to
    /// what's still pending.  Without the reset, a function with a
    /// long sequence Rebuild → many in-place edits could trip the
    /// stall guard prematurely even though every iteration up to that
    /// point was making progress.
    fn rebuild(&mut self) -> Result<()> {
        self.lift_and_seat("rebuild")?;
        self.stall_budget = self.unresolved.len();
        Ok(())
    }

    /// Run the destructive subset and consume `self`, returning the
    /// final graph.
    fn finalize(mut self) -> Result<strider_ir::Function> {
        let pipeline = self.strider.build_destructive_optimizer_pipeline();
        let compact = self.compact;
        let entry = self.function.entry().ok_or_else(|| {
            anyhow::anyhow!("finalize: graph has not been built (entry is None)")
        })?;
        pipeline.run(&mut self.function, entry)?;
        if compact {
            self.function.compact()?;
        }
        Ok(self.function)
    }

    /// Classify every unresolved anchor; partition into
    /// (next_known_targets, in_place_edits).
    #[allow(clippy::type_complexity)]
    fn classify_and_partition(
        &mut self,
    ) -> Result<(
        FxHashMap<PcodeInsnAddr, ResolvedTargets>,
        Vec<(NodeId, ResolvedTargets)>,
    )> {
        let function = &self.function;
        let rom_ref: Option<&dyn ReadOnlyMemory> = self.rom.as_deref();
        // Start from the previous iteration's resolutions so anchors
        // already lowered to switch edges by an earlier Rebuild stay in
        // the known_targets map.  Wiping them would re-introduce the
        // BranchIndirect on the next rebuild and the loop would
        // oscillate between resolved and unresolved.
        let mut next_known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = self.known_targets.clone();
        let mut in_place_edits: Vec<(NodeId, ResolvedTargets)> = Vec::new();
        // Compute known-bits once across all anchors: the function doesn't
        // change between iterations of this loop, so a single pass
        // suffices for every anchor we classify.
        let view = crate::pattern::RewriteCtxView::from_built(function)?;
        let known = crate::opt::analyze_known_bits(view)?;
        for (addr, anchor_output) in &self.unresolved {
            let resolved_opt = classify_anchor(
                view,
                *anchor_output,
                self.lr_vn,
                rom_ref,
                self.stack_vn,
                &known,
            );
            let Some(resolved) = resolved_opt else {
                continue;
            };
            let placeholder_return =
                crate::opt::find_placeholder_return_for_anchor(function.graph(), *anchor_output);
            let can_inplace = match (&resolved, placeholder_return) {
                (ResolvedTargets::LinkRegister, Some(_)) => true,
                (ResolvedTargets::Single(target), Some(_)) => is_tail_call(
                    *target,
                    self.start_addr,
                    self.fn_max_size,
                    self.allow_code_before_start_addr,
                ),
                _ => false,
            };
            if can_inplace
                && let Some(ret) = placeholder_return
            {
                in_place_edits.push((ret, resolved));
                continue;
            }
            next_known.insert(*addr, resolved);
        }
        Ok((next_known, in_place_edits))
    }

    fn apply_in_place_edits(
        &mut self,
        in_place_edits: &[(NodeId, ResolvedTargets)],
    ) -> Result<()> {
        let strider = self.strider;
        let region_index = &self.region_index;
        let per_address_built_ccs = &self.per_address_built_ccs;
        let function = &mut self.function;
        // `Graph::initial_var_for` is maintained on the graph itself
        // (populated by `FunctionBuilder::set_entry_region` at lift
        // time and by `read_or_init_var` for lazily-minted nodes), so
        // there's no longer a per-iteration `preorder()` rebuild here —
        // `read_or_init_var` does an O(1) lookup against the side-table
        // and validates the returned NodeId's use-list to skip zombie
        // `InitialVar` nodes left detached by a previous
        // `FunctionArgDetect`.
        for (placeholder, resolved) in in_place_edits {
            apply_in_place_edit(
                function,
                strider,
                region_index,
                *placeholder,
                resolved,
                per_address_built_ccs,
            )?;
        }
        Ok(())
    }

    /// Filter `self.unresolved` against the post-edit graph: drop
    /// entries whose placeholder Return was detached by an in-place
    /// edit.  No-op when no edits fired (returns the unmodified vec).
    fn recompute_unresolved(
        &mut self,
        in_place_edits: &[(NodeId, ResolvedTargets)],
    ) -> Result<Vec<(PcodeInsnAddr, strider_ir::Value)>> {
        let unresolved = std::mem::take(&mut self.unresolved);
        if in_place_edits.is_empty() {
            return Ok(unresolved);
        }
        Ok(unresolved
            .into_iter()
            .filter(|(_, anchor)| {
                crate::opt::find_placeholder_return_for_anchor(self.function.graph(), *anchor).is_some()
            })
            .collect())
    }
}

/// Decides whether `target` is a tail call — i.e. lies outside the
/// function's address range `[start_addr, start_addr + fn_max_size)`.
/// Delegates to [`strider_lift::cfg::is_addr_tail_call`] so the cfg-time and orchestrator
/// classifications stay in lockstep.
fn is_tail_call(
    target: u64,
    start_addr: strider_lift::cfg::MachineInsnAddr,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
) -> bool {
    strider_lift::cfg::is_addr_tail_call(
        target,
        start_addr.addr,
        fn_max_size,
        allow_code_before_start_addr,
    )
}

fn apply_in_place_edit(
    function: &mut strider_ir::Function,
    strider: &Strider,
    region_index: &RegionIndex,
    placeholder: NodeId,
    resolved: &ResolvedTargets,
    per_address_built_ccs: &FxHashMap<u64, strider_target::BuiltCallingConvention>,
) -> Result<()> {
    match resolved {
        ResolvedTargets::LinkRegister => {
            let ctx = crate::opt::AnchorCallingContext::for_anchor(
                function,
                placeholder,
                strider,
                region_index,
                None,
            )?;
            let _new_return = function.with_rewrite_ctx(|rctx| {
                apply_link_register(rctx, placeholder, &ctx.ret_val_outputs)
            })?;
            Ok(())
        }
        ResolvedTargets::Single(target) => {
            let override_cc = per_address_built_ccs.get(target);
            let ctx = crate::opt::AnchorCallingContext::for_anchor(
                function,
                placeholder,
                strider,
                region_index,
                override_cc,
            )?;
            // Memory-preserving CCs (the override's flag, or the function
            // default when no override is in play) suppress the spliced
            // Call's memory clobber so LoadReadOnly / LoadForward
            // chains stay intact across the tail call.
            let no_memory_clobber = override_cc.map_or_else(
                || strider.calling_convention().no_memory_clobber,
                |cc| cc.no_memory_clobber,
            );
            let new_return = function.with_rewrite_ctx(|rctx| {
                apply_tail_call(
                    rctx,
                    placeholder,
                    *target,
                    &ctx.arg_passing_outputs,
                    &ctx.clobbered_kinds,
                    &ctx.ret_val_outputs,
                    no_memory_clobber,
                )
            })?;
            // When an override was used, record the per-Call clobber
            // varnodes on the spliced Call so pattern queries can
            // recover the right varnode for each clobber slot.  The
            // spliced node is the freshly-created Call adjacent to
            // `new_return`'s ctrl predecessor.  Reuses
            // [`override_clobber_vars`] (also called from
            // [`crate::opt::AnchorCallingContext::for_anchor`]) so the
            // projection over `function.variables` is defined once.
            if let Some(cc) = override_cc
                && let Some(call_id) = locate_spliced_call(function, new_return)
            {
                let clobber_vars: Vec<rsleigh::Vn> =
                    override_clobber_vars(function, cc, strider).collect();
                function.set_call_clobbered_override(call_id, clobber_vars);
                function.set_call_stack_arg_offsets_override(call_id, cc.stack_arg_offsets.clone());
            }
            Ok(())
        }
        ResolvedTargets::Multiple(_) => Err(anyhow!(
            "apply_in_place_edit called with ResolvedTargets::Multiple — caller must route via CFG rebuild"
        )),
    }
}

/// Walks back from a freshly-spliced Return node to find the Call
/// node that `apply_tail_call` inserted as the Return's control
/// predecessor.  Returns `None` if the shape doesn't match the
/// expected `[..ctrl_state..] -> Call -> Return` chain — defensive,
/// since per-address-CC override recording is best-effort and a
/// missed shape is correctness-neutral (the Call still works, just
/// without an override side-table entry).
///
/// Walks two levels to handle both shapes the splicer can produce:
///   * `Call -> Return` (direct): one walk hop.
///   * `Call -> Region -> Return` (region-join): two walk hops.
fn locate_spliced_call(graph: &strider_ir::Graph, ret: NodeId) -> Option<NodeId> {
    let ctrl_in = graph.nth_input(ret, 0)?;
    let (producer, _slot) = graph.output_definition(ctrl_in);
    if matches!(graph.node_kind(producer), strider_ir::node::NodeKind::Call) {
        return Some(producer);
    }
    // Region bridge: walk the Region's first control input
    // and check if THAT producer is a Call.  Mirrors the splice shape
    // when `apply_tail_call`'s freshly-spliced Call feeds an existing
    // Region that the new Return then consumes.
    if matches!(graph.node_kind(producer), strider_ir::node::NodeKind::Region) {
        for cs_in in graph.node_inputs(producer) {
            let (cs_producer, _) = graph.output_definition(cs_in);
            if matches!(graph.node_kind(cs_producer), strider_ir::node::NodeKind::Call) {
                return Some(cs_producer);
            }
        }
    }
    None
}

impl crate::opt::AnchorCallingContext {
    /// Build the calling-convention context for an indirect-branch
    /// placeholder's dispatch site.
    ///
    /// Reads the convention's `arg_passing_regs` / `ret_val_regs` from
    /// the region whose `exit_control` matches the placeholder's
    /// pre-edit control input, falling back to a fresh
    /// `InitialVar(vn)` when a varnode isn't tracked in the region.
    /// The `clobbered_kinds` slot mirrors
    /// `Graph::call_clobbered` so the resulting Call
    /// node's outputs match the canonical
    /// `FunctionBuilder::build_call`-shape.
    ///
    /// `override_cc = Some(cc)` routes arg-passing / ret-val / clobber
    /// computation through `cc` (per-target-address override);
    /// `None` uses the strider's function-default convention.
    fn for_anchor(
        function: &mut strider_ir::Function,
        placeholder: NodeId,
        strider: &Strider,
        region_index: &RegionIndex,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<Self> {
        // When an override is supplied, route arg-passing / ret-val /
        // clobber computation through the override CC instead of the
        // function-default.
        let cc: &strider_target::BuiltCallingConvention = override_cc
            .unwrap_or_else(|| strider.calling_convention());
        let region = region_index.region_for_placeholder(function, placeholder);
        let mut ctx = Self::default();

        // Each `read_or_init_var` call is O(1) against the function's
        // maintained `Graph::initial_var_for` index — no per-iteration
        // arena scan, no per-edit threading.

        // Route `arg_passing_regs` enumeration through the canonical
        // `PositionalArgLayout::register_args` so the ABI-order policy
        // (register slots first, then stack slots) lives in one place.
        // Stack args / clobbers / return-value regs keep the hand-rolled
        // loops — those don't fit the layout's register/stack split.
        let layout = strider_target::PositionalArgLayout::from_convention(cc);
        for (_index, vn) in layout.register_args() {
            // surface unsupported reg sizes as Err instead
            // of silently dropping the slot (which under-models the Call
            // and can cause downstream pattern queries to miss args).
            let out = read_or_init_var(function, region, vn)?;
            ctx.arg_passing_outputs.push(out);
        }
        // Clobber list: with an override, recompute from the override's
        // callee_saved set against the function's tracked variables (via
        // the shared [`override_clobber_vars`] helper, which is also reused
        // by `apply_in_place_edit` after splicing); without, use the
        // precomputed `Graph::call_clobbered` shape.
        //
        // The two branches type-unify via a `SmallVec<[&Vn; 16]>` — stack
        // allocation covers the common case (typical clobber lists are well
        // under 16 entries) and the value only spills to heap on outliers,
        // sparing a `Box<dyn Iterator>` allocation per call on a hot path
        // of the indirect-branch resolution loop.
        let override_clobbers: Vec<rsleigh::Vn>;
        let clobber_iter: smallvec::SmallVec<[&rsleigh::Vn; 16]> = if let Some(cc) = override_cc {
            override_clobbers = override_clobber_vars(function, cc, strider).collect();
            override_clobbers.iter().collect()
        } else {
            function.call_clobbered_regs().iter().collect()
        };
        for vn in clobber_iter {
            // surface unsupported clobber-reg sizes as Err rather than
            // silently defaulting — a size we don't know how to lower
            // would otherwise produce a malformed Call output kind.
            let ty = vn_size_to_node_output_type(vn)?;
            ctx.clobbered_kinds
                .push(strider_ir::node::NodeOutputKind::OutputType(ty));
        }
        // Include BOTH integer and float return-value regs.  The
        // naturally-lifted Return (via `FunctionBuilder`) uses
        // `ret_val_vars()` which combines both, so the synthesised
        // Return must match that arity — otherwise AArch64 q0/q1,
        // x86_64 XMM0/XMM1, MIPS f0/f2, PPC f1/f2, ARM d0/d1 slots
        // silently vanish for indirect-branch-resolved Returns.
        for vn in cc.ret_val_regs.iter().chain(cc.ret_val_regs_float.iter()) {
            let out = read_or_init_var(function, region, *vn)?;
            ctx.ret_val_outputs.push(out);
        }
        Ok(ctx)
    }
}

/// Map a varnode's byte width to the matching [`strider_ir::node::NodeOutputType`].
///
/// Used by the orchestrator's anchor-calling-context plumbing
/// ([`crate::opt::AnchorCallingContext::for_anchor`] for clobber outputs,
/// `read_or_init_var` for freshly-created `InitialVar` nodes) to surface
/// unsupported sizes as a typed error rather than silently dropping the
/// slot.  Every supported CC preset uses sizes ∈ {1, 2, 4, 8, 10, 16,
/// 32, 64} which all map cleanly; the Err arm exists so a future CC
/// addition with an exotic size surfaces the gap immediately.
fn vn_size_to_node_output_type(vn: &rsleigh::Vn) -> Result<strider_ir::node::NodeOutputType> {
    strider_ir::node::NodeOutputType::try_from(vn.size).map_err(|_| {
        anyhow::anyhow!(
            "varnode size {} has no NodeOutputType — calling-convention \
             register {:?} cannot be modelled (supported sizes are 1, 2, 4, \
             8, 10, 16, 32, 64 bytes)",
            vn.size,
            vn,
        )
    })
}

/// Iterate the function-tracked varnodes that are *clobbered* under the
/// per-address override calling convention `cc`.
///
/// Mirrors the body of the `override_cc.is_some()` arm of
/// [`crate::opt::AnchorCallingContext::for_anchor`]'s clobber
/// computation and the post-splice clobber rebuild in
/// [`apply_in_place_edit`] — delegates the actual projection to
/// [`BuiltCallingConvention::clobbers_override_var`] so the
/// `!callee_saved && != stack_ptr` rule lives in exactly one place
/// (mirrored by `FunctionBuilder::build_call_with_cc`).
///
/// Returns owned `Vn`s for caller flexibility (collect into a `Vec` for
/// `set_call_clobbered_override`, or iterate directly to feed
/// `clobbered_kinds`).
fn override_clobber_vars<'a>(
    function: &'a strider_ir::Function,
    cc: &'a strider_target::BuiltCallingConvention,
    strider: &'a Strider,
) -> impl Iterator<Item = rsleigh::Vn> + 'a {
    let stack_vn = strider.calling_convention().stack_vn;
    function
        .variables_map()
        .values()
        .copied()
        .filter(move |v| cc.clobbers_override_var(v, stack_vn))
}

/// Resolve a varnode to its IR value at the placeholder site.
/// Order: (1) region exit `vn_to_value`, (2) existing `InitialVar(vn)`
/// in the graph, (3) freshly-created `InitialVar(vn)`.
///
/// returns an error (instead of silently dropping the
/// varnode) when its byte size has no matching `NodeOutputType`.  In
/// practice every supported CC preset uses sizes ∈ {1, 2, 4, 8, 10,
/// 16, 32, 64} which all map cleanly; the Err arm exists so a future
/// CC addition with an exotic size surfaces the gap immediately
/// instead of producing a Call node with under-modelled inputs.
///
/// # Errors
///
/// Returns `Err` if `vn.size` doesn't map to a `NodeOutputType` or
/// if the freshly-created `InitialVar` doesn't have exactly one
/// output (the `node_signature` invariant guarantees this; the error
/// path exists only for defensive completeness).
fn read_or_init_var(
    function: &mut strider_ir::Function,
    region: Option<&ExitVnToValue>,
    vn: rsleigh::Vn,
) -> Result<NodeOutputId> {
    if let Some(r) = region
        && let Some(&out) = r.get(&vn)
    {
        return Ok(out);
    }
    // Consult the maintained `InitialVar` index.  Skip detached zombies
    // by validating that the registered node's single output still has
    // live uses — a zero-use entry indicates the index points at a
    // detached node, so we fall through and mint a fresh `InitialVar`
    // instead of resurrecting the zombie.
    if let Some(nid) = function.initial_var_for(vn) {
        let [out] = function.node_outputs_exact::<1>(nid).map_err(|e| {
            anyhow!(
                "read_or_init_var: InitialVar({vn:?}) at {nid:?} has wrong output \
                 arity (expected 1): {e}"
            )
        })?;
        if function.output_uses(out).next().is_some() {
            return Ok(out);
        }
    }
    let ty = vn_size_to_node_output_type(&vn)?;
    let nid = function.graph_mut().create_node(
        strider_ir::node::NodeKind::InitialVar(vn),
        [],
        [strider_ir::node::NodeOutputKind::OutputType(ty)],
    );
    let [out] = function.node_outputs_exact::<1>(nid)?;
    function.register_initial_var(vn, nid);
    Ok(out)
}


/// Build the CFG with the strider's arch + the current `known_targets`
/// resolution map.
///
/// Constructs the `OptionsBuilder` from `rom` / link-register /
/// `fn_max_size` / `allow_code_before_start_addr`, installs the
/// strider-analyze mini-IR indirect-branch resolver, and threads the
/// shared decode cache.
#[allow(clippy::too_many_arguments)]
fn build_cfg<R>(
    sleigh: rsleigh::Sleigh<R>,
    strider: &Strider,
    start_addr: strider_lift::cfg::MachineInsnAddr,
    rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    decode_cache: &DecodeCache,
) -> Result<Cfg<R>>
where
    R: rsleigh::MemReader,
{
    let mut opts_builder = OptionsBuilder::new();
    if let Some(rom) = rom {
        opts_builder = opts_builder.set_read_only_memory(rom);
    }
    if let Some(lr) = strider.calling_convention().link_register_vn {
        opts_builder = opts_builder.set_link_register(lr);
    }
    if let Some(max) = fn_max_size {
        opts_builder = opts_builder.set_function_max_size(max);
    }
    if allow_code_before_start_addr {
        opts_builder = opts_builder.allow_code_before_start_addr();
    }
    let cfg_opts = opts_builder.build();

    // Use `for_arch` so both endianness AND `ArchPreset` are derived from the
    // arch atomically.  Earlier ctors (`Builder::new` / `with_endianness`)
    // silently defaulted `preset = X86_64`, causing arch-specific CallOther
    // dispatch (ARM `swi`, AArch64 SMCCC) to be looked up under the wrong
    // preset; those ctors are no longer exposed — `for_arch` is the only
    // public path.
    //
    // Install the strider-analyze mini-IR resolver: without it, the
    // cfg builder treats every `BranchIndirect` as deferred via
    // `UnresolvedIndirectBranch`.  The closure captures nothing
    // (zero-state) — `resolve_indirect_target` is a free function.
    let resolver: strider_lift::cfg::IndirectResolverFn<R> = std::sync::Arc::new(
        |insns, target_vn, sleigh, lr_vn, rom, endianness| {
            crate::indirect_resolver::resolve_indirect_target(
                insns, target_vn, sleigh, lr_vn, rom, endianness,
            )
        },
    );
    Builder::for_arch(&strider.arch, sleigh, start_addr.addr, cfg_opts)
        .with_known_targets(known_targets.clone())
        .with_decode_cache(decode_cache.clone())
        .with_indirect_resolver(resolver)
        .build()
}

/// Union the varnodes from any regions added since the last
/// `scan_new_vns` call into `vn_cache`, then return the sorted set as
/// a `Vec` ready to feed into `strider.analyze_cfg_with`.
///
/// petgraph's `StableDiGraph` allocates monotonic `NodeIndex`s, so
/// `regions().skip(*vn_cache_region_count)` yields exactly the new
/// ones; at iter 0 the cache is empty and every region is scanned.
/// Region splits leave the cache slightly conservative — see the
/// field doc on `LoopState::vn_cache` for why that's safe.
fn scan_new_vns<R>(
    cfg: &Cfg<R>,
    vn_cache: &mut rustc_hash::FxHashSet<rsleigh::Vn>,
    vn_cache_region_count: &mut usize,
) -> Vec<rsleigh::Vn>
where
    R: rsleigh::MemReader,
{
    let starting = *vn_cache_region_count;
    for region in cfg.regions().skip(starting) {
        for wrapped in region.insns.iter() {
            for vn in wrapped.insn.all_vns() {
                vn_cache.insert(vn);
            }
        }
    }
    *vn_cache_region_count = cfg.regions().count();
    let mut all_vns: Vec<rsleigh::Vn> = vn_cache.iter().copied().collect();
    all_vns.sort_unstable_by_key(strider_lift::pcode_lift::vn_sort_key);
    all_vns
}

/// The induced edge set of a `known_targets` map.  Used to test
/// convergence between iterations.
///
/// Each edge is `(anchor_addr, target)` where `target = None` denotes a
/// `LinkRegister` resolution (no successor address — two such anchors at
/// the same `addr` are equivalent regardless of payload) and
/// `target = Some(addr)` denotes a `Single`/`Multiple` resolution to that
/// address.  The `BTreeSet` gives us deterministic sort+dedup in one type
/// (replaces the `EdgeKind { LinkRegister, Target(u64) }` enum + Vec
/// sort+dedup pair).
fn edge_set_of(
    map: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> BTreeSet<(PcodeInsnAddr, Option<u64>)> {
    let mut edges: BTreeSet<(PcodeInsnAddr, Option<u64>)> = BTreeSet::new();
    for (addr, resolved) in map {
        match resolved {
            ResolvedTargets::LinkRegister => {
                edges.insert((*addr, None));
            }
            ResolvedTargets::Single(k) => {
                edges.insert((*addr, Some(*k)));
            }
            ResolvedTargets::Multiple(targets) => {
                for k in targets {
                    edges.insert((*addr, Some(*k)));
                }
            }
        }
    }
    edges
}

/// Renders one HTML viewer per region into `out_dir`.
///
/// `exit_controls` names each region by the `NodeOutputId` that its
/// terminator consumed at lift time — obtain it from
/// [`crate::AnalyzeOutcome::region_exit_controls`].  For each exit:
///
/// 1. Walk backward from the exit's producer via
///    [`strider_ir::walk::region_membership_from_exit`] to collect the
///    region's visualisation membership (control spine, halted at
///    `Region` join nodes, then the data-ancestor closure).
/// 2. Build a `strider_ir::function_dot::FunctionDotDumper` limited to that
///    membership.
/// 3. Write `region_<idx>_<addr>.html` into `out_dir`, where `<idx>` is
///    the region's enumeration index and `<addr>` is the first
///    asm-fingerprint of the exit's producer (zero-padded 16-hex-digit
///    `u64`).  The leading `<idx>` is unconditional: two regions sharing
///    a producer's first asm-fingerprint (e.g. a synthesised region
///    terminator that was stamped from the same lift address as another
///    region's exit) would otherwise produce colliding filenames whose
///    second write silently truncated the first via `std::fs::write`.
///    Regions whose producer carries no asm-fingerprint fall back to
///    `region_<idx>_nofp.html`.
///
/// `out_dir` must exist; this function does not create it.
///
/// `lift_generation` is the snapshot of [`strider_ir::Graph::generation`]
/// taken when the `exit_controls` ids were minted — pass
/// `outcome.function.generation()`.  If the live `graph`'s
/// generation has advanced since (the graph was compacted), the
/// `exit_controls` ids are stale and dereferencing them would address
/// the wrong region; this function returns a typed error instead.
///
/// # Errors
///
/// Returns an error if [`strider_ir::Graph::dot_dumper`] fails (graph
/// not built), if HTML rendering fails, if a write to `out_dir` fails,
/// or if the graph's generation no longer matches `lift_generation`.
pub fn dump_per_region<R, I>(
    function: &strider_ir::Function,
    exit_controls: I,
    lift_generation: u64,
    sleigh: &rsleigh::Sleigh<R>,
    out_dir: &std::path::Path,
) -> Result<()>
where
    R: rsleigh::MemReader,
    I: IntoIterator<Item = NodeOutputId>,
{
    if function.generation() != lift_generation {
        return Err(anyhow!(
            "dump_per_region: function generation {} does not match lift snapshot {}; \
             the function was compacted after lift and exit_controls are stale",
            function.generation(),
            lift_generation,
        ));
    }
    for (idx, exit_control) in exit_controls.into_iter().enumerate() {
        let membership = strider_ir::walk::region_membership_from_exit(function, exit_control);
        // Construct a fresh dumper per region via the public
        // `Graph::dot_dumper` + `with_node_filter` chain.  The dumper
        // borrows from `function` / `sleigh`, so we can't reuse one across
        // iterations (each `with_node_filter` consumes the value).
        let dumper = function.dot_dumper(sleigh)?.with_node_filter(membership);

        let producer = function.get_node_from_output(exit_control);
        // Include `idx` unconditionally: two regions whose producers
        // share a first asm-fingerprint would otherwise collide via
        // `std::fs::write` (silent overwrite).
        let addr_part: String = function
            .asm_fingerprint(producer)
            .first()
            .map_or_else(|| "nofp".to_string(), |a| format!("{a:016x}"));
        let path = out_dir.join(format!("region_{idx}_{addr_part}.html"));
        ::dot::GraphDot::new(dumper, ::dot::DotStyle::dark())
            .dump_as_html(&path)
            .map_err(|e| {
                anyhow!("dump_per_region: write {} failed: {e}", path.display())
            })?;
    }
    Ok(())
}

/// Writes an HTML viewer for the subgraph within `depth` hops of
/// `anchor` (forward + backward) to `out_path`.
///
/// Uses [`strider_ir::walk::collect_neighborhood`] to build the visible
/// node set and renders via [`strider_ir::Graph::dot_dumper`]'s
/// `with_node_filter` chain.  Useful for "focus on this node" debug
/// dumps when the whole-function view is too dense.
///
/// `depth = 0` produces a singleton viewer; `depth = 1` includes
/// immediate predecessors and successors; larger depths walk further.
///
/// # Errors
///
/// Returns an error when `anchor` is not a live node in `function` (e.g.
/// a stale id from a pre-compaction snapshot, or a foreign id from a
/// different `Graph`), when dumper construction fails (function not
/// built), HTML rendering fails, or the write to `out_path` fails.
pub fn dump_neighborhood<R>(
    function: &strider_ir::Function,
    anchor: NodeId,
    depth: u32,
    sleigh: &rsleigh::Sleigh<R>,
    out_path: &std::path::Path,
) -> Result<()>
where
    R: rsleigh::MemReader,
{
    if !function.has_node(anchor) {
        return Err(anyhow!(
            "dump_neighborhood: anchor {anchor:?} is not a live node in this function \
             (stale id from a pre-compaction snapshot, or a foreign id)",
        ));
    }
    let visible = strider_ir::walk::collect_neighborhood(function, anchor, depth);
    let dumper = function.dot_dumper(sleigh)?.with_node_filter(visible);
    ::dot::GraphDot::new(dumper, ::dot::DotStyle::dark())
        .dump_as_html(out_path)
        .map_err(|e| {
            anyhow!("dump_neighborhood: write {} failed: {e}", out_path.display())
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_lift::cfg::MachineInsnAddr;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr { machine_addr: MachineInsnAddr::from(machine), insn_index: 0 }
    }

    fn make_strider_x86_64() -> Strider {
        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .expect("x86_64_systemv preset must be registered");
        Strider::new(arch, regs, cc).expect("strider")
    }


    // ── apply_stall_guard tests ──────────────────────────────────
    //
    // These tests pin the fix (`>=` → `>`) by exercising the
    // stall-guard behavior directly via the extracted helper.  Each
    // case names the relevant scenario from the orchestrator's
    // fixed-point loop.

    #[test]
    fn apply_stall_guard_no_change_in_count_does_not_consume_budget() {
        // regression: a count-stable in-place-only
        // iteration (one anchor resolved, one new placeholder
        // materialised) is legitimate progress and must NOT consume
        // budget.  Pre-fix (`>=`) this ate one budget per stable
        // iteration; post-fix (`>`) the budget stays full.
        let mut budget = 3usize;
        for _ in 0..5 {
            apply_stall_guard(&mut budget, /* edge_set_changed */ false, 4, 4)
                .expect("count-stable iteration must not error");
        }
        assert_eq!(budget, 3, "budget must stay full across 5 count-stable iterations");
    }

    #[test]
    fn apply_stall_guard_count_decrease_does_not_consume_budget() {
        // The natural progress shape: count strictly decreases.  Budget
        // stays full.
        let mut budget = 3usize;
        apply_stall_guard(&mut budget, false, 3, 4)
            .expect("count-decrease must not error");
        assert_eq!(budget, 3);
    }

    #[test]
    fn apply_stall_guard_count_growth_consumes_budget() {
        // Strictly-growing count (resolver producing more anchors than
        // it resolves) is the real stall pathology.  Each growth step
        // decrements budget; reaching zero raises Err.
        let mut budget = 2usize;
        // Iter 1: 4 → 5 (grew by 1). Budget: 2 → 1.
        apply_stall_guard(&mut budget, false, 5, 4).expect("first growth ok");
        assert_eq!(budget, 1);
        // Iter 2: 5 → 6. Budget: 1 → 0.
        apply_stall_guard(&mut budget, false, 6, 5).expect("second growth ok");
        assert_eq!(budget, 0);
        // Iter 3: 6 → 7. Budget: 0 — bail.
        let err = apply_stall_guard(&mut budget, false, 7, 6)
            .expect_err("third growth must surface the stall");
        assert!(
            err.to_string().contains("in-place edits stalled"),
            "got: {err}"
        );
    }

    #[test]
    fn apply_stall_guard_edge_set_change_skips_check() {
        // When edge_set_changed (Rebuild path), the stall guard is
        // entirely skipped.  Budget stays untouched even on growth.
        let mut budget = 1usize;
        apply_stall_guard(&mut budget, /* edge_set_changed */ true, 100, 1)
            .expect("rebuild path skips stall check");
        assert_eq!(budget, 1, "edge-set change must not consume budget");
    }

    #[test]
    fn apply_stall_guard_zero_budget_with_no_growth_is_ok() {
        // Budget 0 + no growth = no stall fires.  Documents that
        // exhausted budget plus benign progress remains progress.
        let mut budget = 0usize;
        apply_stall_guard(&mut budget, false, 4, 4).expect("count-stable + 0-budget ok");
        apply_stall_guard(&mut budget, false, 3, 4).expect("count-decrease + 0-budget ok");
        apply_stall_guard(&mut budget, true, 100, 4).expect("edge-change + 0-budget ok");
    }

    // ── existing edge-set tests ───────────────────────────────────────────

    #[test]
    fn edge_set_of_empty_map_is_empty() {
        let map: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        assert!(edge_set_of(&map).is_empty());
    }

    #[test]
    fn edge_set_of_single_link_register_resolution() {
        let mut map: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        map.insert(pcode_addr(0x1000), ResolvedTargets::LinkRegister);
        let edges = edge_set_of(&map);
        assert_eq!(edges.len(), 1);
        assert!(edges.contains(&(pcode_addr(0x1000), None)));
    }

    #[test]
    fn edge_set_of_single_resolution_matches_single_edge() {
        let mut map: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        map.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        let edges = edge_set_of(&map);
        let expected: BTreeSet<(PcodeInsnAddr, Option<u64>)> =
            std::iter::once((pcode_addr(0x1000), Some(0x2000))).collect();
        assert_eq!(edges, expected);
    }

    #[test]
    fn edge_set_of_multiple_resolution_matches_n_edges() {
        let mut map: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        map.insert(
            pcode_addr(0x1000),
            ResolvedTargets::Multiple(vec![0x2000, 0x3000, 0x4000]),
        );
        let edges = edge_set_of(&map);
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn edge_set_is_order_independent() {
        let mut a: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        a.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        a.insert(pcode_addr(0x3000), ResolvedTargets::Single(0x4000));
        let mut b: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        b.insert(pcode_addr(0x3000), ResolvedTargets::Single(0x4000));
        b.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        assert_eq!(edge_set_of(&a), edge_set_of(&b));
    }

    #[test]
    fn edge_set_dedups_duplicate_targets_in_multiple() {
        let mut map: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        map.insert(
            pcode_addr(0x1000),
            ResolvedTargets::Multiple(vec![0x2000, 0x2000, 0x2000]),
        );
        assert_eq!(edge_set_of(&map).len(), 1);
    }

    #[test]
    fn is_tail_call_target_below_start_addr_is_tail_call() {
        let _strider = make_strider_x86_64();
        assert!(is_tail_call(0x0fff, 0x1000u64.into(), None, false));
        assert!(!is_tail_call(0x1000, 0x1000u64.into(), None, false));
        assert!(!is_tail_call(0x1001, 0x1000u64.into(), None, false));
    }

    #[test]
    fn is_tail_call_allow_code_before_start_addr_disables_below_check() {
        let _strider = make_strider_x86_64();
        assert!(!is_tail_call(0x0fff, 0x1000u64.into(), None, true));
    }

    #[test]
    fn is_tail_call_above_fn_max_size_is_tail_call() {
        let _strider = make_strider_x86_64();
        // `[start_addr, start_addr + fn_max_size)` is the in-function
        // half-open range: target == end_exclusive is a tail call.
        assert!(is_tail_call(0x1100, 0x1000u64.into(), Some(0x100), false));
        assert!(!is_tail_call(0x10ff, 0x1000u64.into(), Some(0x100), false));
        assert!(is_tail_call(0x2000, 0x1000u64.into(), Some(0x100), false));
    }

    #[test]
    fn is_tail_call_no_fn_max_size_means_above_is_intra_fn() {
        let _strider = make_strider_x86_64();
        assert!(!is_tail_call(0xffff_ffff_ffff_ffff, 0x1000u64.into(), None, false));
    }

    #[test]
    fn is_tail_call_fn_max_size_saturates_on_overflow() {
        let _strider = make_strider_x86_64();
        assert!(is_tail_call(
            u64::MAX,
            (u64::MAX - 5).into(),
            Some(0x100),
            false,
        ));
    }

    #[test]
    fn iteration_cap_formula_handles_zero_pending_branches() {
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
        let cap = 2usize.saturating_mul(usize::MAX).saturating_add(4);
        assert_eq!(cap, usize::MAX);
    }
}
