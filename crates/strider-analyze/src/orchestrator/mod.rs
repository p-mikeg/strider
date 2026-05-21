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
//!    [`crate::opt::indirect_branch_resolve::classify_anchor_with_rom_and_sp`].
//! 5. Apply in-place IR edits for terminal classifications:
//!    [`crate::opt::apply_link_register`] for `LinkRegister`,
//!    [`crate::opt::apply_tail_call`] for `Single(K)` where `K` is outside
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

use std::collections::{BTreeSet, HashMap};

use anyhow::{anyhow, bail, Result};

use strider_lift::cfg::{Builder, Cfg, DecodeCache, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::{NodeId, NodeOutputId};
use crate::opt::ReadOnlyMemory;

use crate::errors::UnresolvedIndirectBranch;
use crate::opt::indirect_branch_resolve::{
    apply_link_register, apply_tail_call, classify_anchor_with_rom_and_sp,
};
use crate::strider::Strider;
use crate::RegionLiftHandles;

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
    /// reachable from `entry` via [`strider_ir::walk::walk_graph`].  Default
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
    pub per_address_ccs: HashMap<u64, strider_target::CallingConvention>,
}

/// Internal view of [`Config`] without the Sleigh handle — see
/// [`LoopState`] for why the orchestrator threads the Sleigh
/// separately.
struct RunOpts<'a> {
    strider: &'a Strider,
    start_addr: strider_lift::cfg::MachineInsnAddr,
    rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    compact: bool,
    /// Pre-resolved per-target-address CC overrides.  See the
    /// [`Config::per_address_ccs`] doc.  Resolved once at
    /// `LoopState::new` so any unresolved register name surfaces
    /// before iteration starts.
    per_address_built_ccs: HashMap<u64, strider_target::BuiltCallingConvention>,
}

/// Per-iteration index built from a lift's [`RegionLiftHandles`]
/// snapshot.  Maps a region's exit-control `NodeOutputId` to the
/// region's exit `vn_to_value` table — what
/// [`build_anchor_calling_context`] needs to thread ABI varnodes
/// through an in-place edit.
/// Maps a region's exit-control `NodeOutputId` to the region's exit
/// `vn_to_value` table.  The map is `Arc`-shared with each
/// [`RegionLiftHandles::exit_vn_to_value`] entry — never mutated
/// post-build, so shared ownership is safe.
type ExitVnToValue = std::sync::Arc<HashMap<rsleigh::Vn, NodeOutputId>>;

struct RegionIndex {
    by_exit_control: HashMap<NodeOutputId, ExitVnToValue>,
}

impl RegionIndex {
    fn from_handles(handles: &[RegionLiftHandles]) -> Self {
        let mut by_exit_control = HashMap::with_capacity(handles.len());
        for h in handles {
            by_exit_control.insert(h.exit_control, std::sync::Arc::clone(&h.exit_vn_to_value));
        }
        Self { by_exit_control }
    }

    fn region_for_placeholder(
        &self,
        graph: &strider_ir::BuiltFunctionGraph,
        placeholder: NodeId,
    ) -> Option<&ExitVnToValue> {
        let inputs: Vec<_> = graph.node_inputs(placeholder).into_iter().collect();
        let ctrl_in = *inputs.first()?;
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
pub fn run<R>(config: Config<'_, R>) -> Result<strider_ir::BuiltFunctionGraph>
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
/// real `LoopState` (which requires a `Sleigh<R>`, `BuiltFunctionGraph`,
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
    opts: RunOpts<'a>,
    /// Accumulator of IR-level indirect-branch resolver resolutions across iterations.
    /// Monotonically grows: once an anchor's targets land here, the
    /// CFG-rebuild path keeps using them.  Per-iteration classifications
    /// overlay this map (so an upgrade like
    /// `Single(K1) → Multiple([K1, K2])` overwrites the entry), but
    /// anchors that are no longer in the per-iteration `unresolved`
    /// list (because the previous Rebuild lowered them to switch
    /// edges) MUST stay — wiping them re-introduces the placeholder
    /// on the next rebuild and the loop diverges.
    known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>,
    /// The Sleigh handle we thread through every iteration.  Initialised
    /// from `Config::sleigh` at construction; consumed by
    /// `Builder::for_arch` per iteration and harvested back from the
    /// resulting `Cfg::into_sleigh()`.  `None` only momentarily inside
    /// `build_lift_stable`.
    sleigh: Option<rsleigh::Sleigh<R>>,
    /// The current optimised IR graph.
    graph: Option<strider_ir::BuiltFunctionGraph>,
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
    sp_vn: Option<rsleigh::Vn>,
    /// Decode cache shared across CFG rebuilds.  The Sleigh handle
    /// persists for the whole `run`, so this cache stays valid for
    /// every iteration; threaded into each fresh `strider_lift::cfg::Builder` so
    /// machine-instruction decodes are paid once per address per run.
    decode_cache: DecodeCache,
    // TODO: remove after incremental indirect-resolve lands —
    // see docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md
    /// Cached set of varnodes seen so far across all CFG iterations.
    /// `find_all_unique_vns` would otherwise re-scan every region's
    /// every instruction's every varnode on every Rebuild iteration;
    /// here we only scan the regions added since the previous
    /// iteration (petgraph's `StableDiGraph` allocates monotonic
    /// `NodeIndex`s, so the new regions sit at indices
    /// `[prev_count..current_count)`).
    ///
    /// The set is conservative under region splits: a split region's
    /// original vns stay in the cache even if the original's insn
    /// list got truncated.  Over-tracking a vn allocates one extra
    /// `InitialVar` (cheap) and never miscompiles.
    vn_cache: std::collections::HashSet<rsleigh::Vn>,
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
        let sp_vn = Some(config.strider.calling_convention().stack_ptr_vn);
        // Pre-resolve per-address CC overrides against the same Sleigh
        // register table the function-default CC was built against.
        let per_address_built_ccs: HashMap<u64, strider_target::BuiltCallingConvention> =
            if config.per_address_ccs.is_empty() {
                HashMap::new()
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
            known_targets: HashMap::new(),
            graph: None,
            unresolved: Vec::new(),
            pending_at_iter_0: 0,
            stall_budget: 0,
            region_index: RegionIndex {
                by_exit_control: HashMap::new(),
            },
            lr_vn,
            sp_vn,
            decode_cache: DecodeCache::new(),
            vn_cache: std::collections::HashSet::new(),
            vn_cache_region_count: 0,
            opts: RunOpts {
                strider: config.strider,
                start_addr: config.start_addr,
                rom: config.rom,
                fn_max_size: config.fn_max_size,
                allow_code_before_start_addr: config.allow_code_before_start_addr,
                compact: config.compact,
                per_address_built_ccs,
            },
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
        let (graph, unresolved, region_index, sleigh) = build_lift_stable(
            sleigh,
            &self.opts,
            &self.known_targets,
            &self.decode_cache,
            &mut self.vn_cache,
            &mut self.vn_cache_region_count,
        )?;
        self.sleigh = Some(sleigh);
        self.region_index = region_index;
        self.graph = Some(graph);
        self.unresolved = unresolved;
        Ok(())
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
                return Err(UnresolvedIndirectBranch { addr }.into());
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
        let pipeline = self.opts.strider.build_stable_optimizer_pipeline();
        let graph = self.graph_mut()?;
        let entry = graph.entry();
        pipeline.run(graph.graph_mut(), entry)?;
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
    fn finalize(mut self) -> Result<strider_ir::BuiltFunctionGraph> {
        let pipeline = self.opts.strider.build_destructive_optimizer_pipeline();
        let compact = self.opts.compact;
        let graph = self.graph_mut()?;
        let entry = graph.entry();
        pipeline.run(graph.graph_mut(), entry)?;
        if compact {
            graph.compact()?;
        }
        self.graph
            .take()
            .ok_or_else(|| anyhow!("orchestrator finalize: graph already consumed"))
    }

    /// Classify every unresolved anchor; partition into
    /// (next_known_targets, in_place_edits).
    #[allow(clippy::type_complexity)]
    fn classify_and_partition(
        &mut self,
    ) -> Result<(
        HashMap<PcodeInsnAddr, ResolvedTargets>,
        Vec<(NodeId, ResolvedTargets)>,
    )> {
        let graph = self
            .graph
            .as_ref()
            .ok_or_else(|| anyhow!("orchestrator classify: graph not initialised"))?;
        let rom_ref: Option<&dyn ReadOnlyMemory> = self.opts.rom.as_deref();
        // Start from the previous iteration's resolutions so anchors
        // already lowered to switch edges by an earlier Rebuild stay in
        // the known_targets map.  Wiping them would re-introduce the
        // BranchIndirect on the next rebuild and the loop would
        // oscillate between resolved and unresolved.
        let mut next_known: HashMap<PcodeInsnAddr, ResolvedTargets> = self.known_targets.clone();
        let mut in_place_edits: Vec<(NodeId, ResolvedTargets)> = Vec::new();
        // Compute known-bits once across all anchors: the graph doesn't
        // change between iterations of this loop, so a single pass
        // suffices for every anchor we classify.
        let view: crate::pattern::RewriteCtxView<'_> = graph.into();
        let known = crate::opt::analyze_known_bits(view)?;
        for (addr, anchor_output) in &self.unresolved {
            let resolved_opt = classify_anchor_with_rom_and_sp(
                view,
                *anchor_output,
                self.lr_vn,
                rom_ref,
                self.sp_vn,
                &known,
            );
            let Some(resolved) = resolved_opt else {
                continue;
            };
            let placeholder_return =
                crate::opt::find_placeholder_return_for_anchor(graph.graph(), *anchor_output);
            let can_inplace = match (&resolved, placeholder_return) {
                (ResolvedTargets::LinkRegister, Some(_)) => true,
                (ResolvedTargets::Single(target), Some(_)) => {
                    is_tail_call(*target, &self.opts)
                }
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
        let strider = self.opts.strider;
        let region_index = &self.region_index;
        let per_address_built_ccs = &self.opts.per_address_built_ccs;
        let graph = self
            .graph
            .as_mut()
            .ok_or_else(|| anyhow!("orchestrator: graph not initialised"))?;
        // Build the InitialVar lookup ONCE per iteration and pass it
        // through to every apply_in_place_edit so per-edit cost drops
        // from O(N) (a full all_node_ids scan inside
        // build_anchor_calling_context) to O(1) per varnode read.
        // read_or_init_var inserts new entries as it creates fresh
        // InitialVar nodes, so the index stays consistent across edits.
        //
        // Use `preorder` (reachable-only) rather than `all_node_ids` so
        // zombie `InitialVar` nodes left detached by a previous
        // `FunctionArgDetect` don't get re-indexed and resurrected:
        // `read_or_init_var` would return a zombie's output and wire it
        // straight into a fresh Call's input list, breaking
        // `FunctionArgDetect`'s post-detection invariant that all
        // argument-register reads flow through `FunctionArg` nodes.
        let mut initial_var_index: HashMap<rsleigh::Vn, NodeOutputId> = HashMap::new();
        for nid in graph.preorder() {
            if let strider_ir::node::NodeKind::InitialVar(existing) = graph.node_kind(nid) {
                // InitialVar's signature is `[]; outputs: [Value]` —
                // exactly one output.  A non-1 count is a graph-shape
                // bug (zombie or malformed); surfacing it as Err
                // prevents `read_or_init_var` from later resurrecting
                // the malformed node and silently producing wrong IR.
                let [out] = graph.node_outputs_exact::<1>(nid).map_err(|e| {
                    anyhow!(
                        "apply_in_place_edits: InitialVar({existing:?}) has wrong output \
                         arity (expected 1): {e}"
                    )
                })?;
                initial_var_index.insert(*existing, out);
            }
        }
        for (placeholder, resolved) in in_place_edits {
            apply_in_place_edit(
                graph,
                strider,
                region_index,
                *placeholder,
                resolved,
                per_address_built_ccs,
                &mut initial_var_index,
            )?;
        }
        Ok(())
    }

    /// Filter `self.unresolved` against the post-edit graph: drop
    /// entries whose placeholder Return was detached by an in-place
    /// edit.  No-op when no edits fired (returns the unmodified vec).
    ///
    /// Returns `Err` when the loop's graph has somehow been cleared while
    /// in-place edits are pending — that's a state-machine bug in the
    /// orchestrator (the graph must be populated before edits can fire),
    /// and silently returning an empty vec would mask it.
    fn recompute_unresolved(
        &mut self,
        in_place_edits: &[(NodeId, ResolvedTargets)],
    ) -> Result<Vec<(PcodeInsnAddr, strider_ir::Value)>> {
        let unresolved = std::mem::take(&mut self.unresolved);
        if in_place_edits.is_empty() {
            return Ok(unresolved);
        }
        let graph = self.graph.as_ref().ok_or_else(|| {
            anyhow!(
                "LoopState::recompute_unresolved: in_place_edits is non-empty but \
                 self.graph is None — orchestrator state machine invariant broken"
            )
        })?;
        Ok(unresolved
            .into_iter()
            .filter(|(_, anchor)| {
                crate::opt::find_placeholder_return_for_anchor(graph.graph(), *anchor).is_some()
            })
            .collect())
    }

    fn graph_mut(&mut self) -> Result<&mut strider_ir::BuiltFunctionGraph> {
        self.graph
            .as_mut()
            .ok_or_else(|| anyhow!("orchestrator: graph not initialised"))
    }
}

/// Decides whether `target` is a tail call — i.e. lies outside the
/// function's address range `[start_addr, start_addr + fn_max_size)`.
/// Delegates to [`strider_lift::cfg::is_addr_tail_call`] so the cfg-time and orchestrator
/// classifications stay in lockstep.
fn is_tail_call(target: u64, opts: &RunOpts<'_>) -> bool {
    strider_lift::cfg::is_addr_tail_call(
        target,
        opts.start_addr.addr,
        opts.fn_max_size,
        opts.allow_code_before_start_addr,
    )
}

fn apply_in_place_edit(
    graph: &mut strider_ir::BuiltFunctionGraph,
    strider: &Strider,
    region_index: &RegionIndex,
    placeholder: NodeId,
    resolved: &ResolvedTargets,
    per_address_built_ccs: &HashMap<u64, strider_target::BuiltCallingConvention>,
    initial_var_index: &mut HashMap<rsleigh::Vn, NodeOutputId>,
) -> Result<()> {
    match resolved {
        ResolvedTargets::LinkRegister => {
            let ctx = build_anchor_calling_context(
                graph,
                placeholder,
                strider,
                region_index,
                None,
                initial_var_index,
            )?;
            apply_link_register(
                &mut crate::pattern::RewriteCtx::for_built(graph),
                placeholder,
                &ctx.ret_val_outputs,
            )?;
            Ok(())
        }
        ResolvedTargets::Single(target) => {
            let override_cc = per_address_built_ccs.get(target);
            let ctx = build_anchor_calling_context(
                graph,
                placeholder,
                strider,
                region_index,
                override_cc,
                initial_var_index,
            )?;
            let new_return = apply_tail_call(
                &mut crate::pattern::RewriteCtx::for_built(graph),
                placeholder,
                *target,
                &ctx.arg_passing_outputs,
                &ctx.clobbered_kinds,
                &ctx.ret_val_outputs,
            )?;
            // When an override was used, record the per-Call clobber
            // varnodes on the spliced Call so pattern queries can
            // recover the right varnode for each clobber slot.  The
            // spliced node is the freshly-created Call adjacent to
            // `new_return`'s ctrl predecessor.  Reuses
            // [`override_clobber_vars`] (also called from
            // [`build_anchor_calling_context`]) so the projection over
            // `graph.variables` is defined once.
            if let Some(cc) = override_cc
                && let Some(call_id) = locate_spliced_call(graph, new_return)
            {
                let clobber_vars: Vec<rsleigh::Vn> =
                    override_clobber_vars(graph, cc, strider).collect();
                graph.graph_mut().set_call_clobbered_override(call_id, clobber_vars);
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
///   * `Call -> ControlState -> Return` (region-join): two walk hops.
fn locate_spliced_call(graph: &strider_ir::BuiltFunctionGraph, ret: NodeId) -> Option<NodeId> {
    let inputs: Vec<_> = graph.node_inputs(ret).into_iter().collect();
    let ctrl_in = *inputs.first()?;
    let (producer, _slot) = graph.output_definition(ctrl_in);
    if matches!(graph.node_kind(producer), strider_ir::node::NodeKind::Call) {
        return Some(producer);
    }
    // ControlState bridge: walk the ControlState's first control input
    // and check if THAT producer is a Call.  Mirrors the splice shape
    // when `apply_tail_call`'s freshly-spliced Call feeds an existing
    // ControlState that the new Return then consumes.
    if matches!(graph.node_kind(producer), strider_ir::node::NodeKind::ControlState) {
        let cs_inputs: Vec<_> = graph.node_inputs(producer).into_iter().collect();
        for cs_in in cs_inputs {
            let (cs_producer, _) = graph.output_definition(cs_in);
            if matches!(graph.node_kind(cs_producer), strider_ir::node::NodeKind::Call) {
                return Some(cs_producer);
            }
        }
    }
    None
}

/// Build the calling-convention context for the placeholder's
/// dispatch site.
///
/// Reads the convention's `arg_passing_regs` / `ret_val_regs` from the
/// region whose `exit_control` matches the placeholder's pre-edit
/// control input, falling back to a fresh `InitialVar(vn)` when a
/// varnode isn't tracked in the region.  The `clobbered_kinds` slot
/// mirrors `BuiltFunctionGraph::call_clobbered` so the resulting Call
/// node's outputs match the canonical
/// `FunctionBuilder::build_call`-shape.
fn build_anchor_calling_context(
    graph: &mut strider_ir::BuiltFunctionGraph,
    placeholder: NodeId,
    strider: &Strider,
    region_index: &RegionIndex,
    override_cc: Option<&strider_target::BuiltCallingConvention>,
    initial_var_index: &mut HashMap<rsleigh::Vn, NodeOutputId>,
) -> Result<crate::opt::AnchorCallingContext> {
    // When an override is supplied, route arg-passing / ret-val /
    // clobber computation through the override CC instead of the
    // function-default.
    let cc: &strider_target::BuiltCallingConvention = override_cc
        .unwrap_or_else(|| strider.calling_convention());
    let region = region_index.region_for_placeholder(graph, placeholder);
    let mut ctx = crate::opt::AnchorCallingContext::default();

    // `initial_var_index` is built once per orchestrator iteration (in
    // `apply_in_place_edits`) and threaded through.  Per-edit cost is
    // O(arg_count) instead of the previous O(N) arena scan.

    for vn in &cc.arg_passing_regs {
        // surface unsupported reg sizes as Err instead
        // of silently dropping the slot (which under-models the Call
        // and can cause downstream pattern queries to miss args).
        let out = read_or_init_var(graph, region, initial_var_index, *vn)?;
        ctx.arg_passing_outputs.push(out);
    }
    // Clobber list: with an override, recompute from the override's
    // callee_saved set against the function's tracked variables (via
    // the shared [`override_clobber_vars`] helper, which is also reused
    // by `apply_in_place_edit` after splicing); without, use the
    // precomputed `BuiltFunctionGraph::call_clobbered` shape.
    //
    // The two branches type-unify via a `SmallVec<[&Vn; 16]>` — stack
    // allocation covers the common case (typical clobber lists are well
    // under 16 entries) and the value only spills to heap on outliers,
    // sparing a `Box<dyn Iterator>` allocation per call on a hot path
    // of the indirect-branch resolution loop.
    let override_clobbers: Vec<rsleigh::Vn>;
    let clobber_iter: smallvec::SmallVec<[&rsleigh::Vn; 16]> = if let Some(cc) = override_cc {
        override_clobbers = override_clobber_vars(graph, cc, strider).collect();
        override_clobbers.iter().collect()
    } else {
        graph.call_clobbered_regs().iter().collect()
    };
    for vn in clobber_iter {
        // surface unsupported clobber-reg sizes as Err rather than
        // silently defaulting — a size we don't know how to lower
        // would otherwise produce a malformed Call output kind.
        let ty = vn_size_to_node_output_type(vn)?;
        ctx.clobbered_kinds
            .push(strider_ir::node::NodeOutputKind::OutputType(ty));
    }
    for vn in &cc.ret_val_regs {
        let out = read_or_init_var(graph, region, initial_var_index, *vn)?;
        ctx.ret_val_outputs.push(out);
    }
    Ok(ctx)
}

/// Map a varnode's byte width to the matching [`strider_ir::node::NodeOutputType`].
///
/// Used by the orchestrator's anchor-calling-context plumbing
/// (`build_anchor_calling_context` for clobber outputs,
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
/// [`build_anchor_calling_context`]'s clobber computation and the
/// post-splice clobber rebuild in [`apply_in_place_edit`] — extracted so
/// the same projection (`!callee_saved && != stack_ptr`) is defined in
/// exactly one place.
///
/// Returns owned `Vn`s for caller flexibility (collect into a `Vec` for
/// `set_call_clobbered_override`, or iterate directly to feed
/// `clobbered_kinds`).
fn override_clobber_vars<'a>(
    graph: &'a strider_ir::BuiltFunctionGraph,
    cc: &'a strider_target::BuiltCallingConvention,
    strider: &'a Strider,
) -> impl Iterator<Item = rsleigh::Vn> + 'a {
    let stack_ptr_vn = strider.calling_convention().stack_ptr_vn;
    graph
        .variables_map()
        .values()
        .copied()
        .filter(move |v| !cc.callee_saved_regs.contains(v) && *v != stack_ptr_vn)
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
    graph: &mut strider_ir::BuiltFunctionGraph,
    region: Option<&ExitVnToValue>,
    initial_var_index: &mut HashMap<rsleigh::Vn, NodeOutputId>,
    vn: rsleigh::Vn,
) -> Result<NodeOutputId> {
    if let Some(r) = region
        && let Some(&out) = r.get(&vn)
    {
        return Ok(out);
    }
    if let Some(&out) = initial_var_index.get(&vn) {
        return Ok(out);
    }
    let ty = vn_size_to_node_output_type(&vn)?;
    let nid = graph.graph_mut().create_node(
        strider_ir::node::NodeKind::InitialVar(vn),
        [],
        [strider_ir::node::NodeOutputKind::OutputType(ty)],
    );
    let [out] = graph.node_outputs_exact::<1>(nid)?;
    initial_var_index.insert(vn, out);
    Ok(out)
}

/// Build the CFG, lift to IR, run the stable optimiser subset.
/// Returns `(graph, unresolved, region_index, sleigh)` so the caller
/// can re-use the harvested Sleigh handle across iterations.
#[allow(clippy::type_complexity)]
fn build_lift_stable<R>(
    sleigh: rsleigh::Sleigh<R>,
    opts: &RunOpts<'_>,
    known_targets: &HashMap<PcodeInsnAddr, ResolvedTargets>,
    decode_cache: &DecodeCache,
    vn_cache: &mut std::collections::HashSet<rsleigh::Vn>,
    vn_cache_region_count: &mut usize,
) -> Result<(
    strider_ir::BuiltFunctionGraph,
    Vec<(PcodeInsnAddr, strider_ir::Value)>,
    RegionIndex,
    rsleigh::Sleigh<R>,
)>
where
    R: rsleigh::MemReader,
{
    let mut opts_builder = OptionsBuilder::new();
    if let Some(rom) = opts.rom.clone() {
        opts_builder = opts_builder.set_read_only_memory(rom);
    }
    if let Some(lr) = opts.strider.calling_convention().link_register_vn {
        opts_builder = opts_builder.set_link_register(lr);
    }
    if let Some(max) = opts.fn_max_size {
        opts_builder = opts_builder.set_function_max_size(max);
    }
    if opts.allow_code_before_start_addr {
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
    // Wrap the cfg build failure as `LiftError` so the strider-py
    // boundary can classify it via a typed downcast instead of a
    // substring scan over the formatted error chain.  Skipping the
    // bare `?` here would let a `Builder::build()` failure (sleigh
    // decode, region overlap, unresolved indirect branch on the
    // strict path, etc.) propagate as a plain `anyhow::Error` and
    // get bucketed under the generic `StriderError` at the boundary.
    let cfg: Cfg<R> = Builder::for_arch(&opts.strider.arch, sleigh, opts.start_addr.addr, cfg_opts)
        .with_known_targets(known_targets.clone())
        .with_decode_cache(decode_cache.clone())
        .with_indirect_resolver(resolver)
        .build()
        .map_err(strider_lift::LiftError::wrap)?;

    // Vn cache: scan only the regions added since the previous
    // iteration (petgraph's StableDiGraph allocates monotonic
    // NodeIndexes, so `regions().skip(prev_count)` yields exactly
    // the new ones).  At iter 0, scans every region.  Region splits
    // leave the cache slightly conservative — see the field doc on
    // LoopState::vn_cache for why that's safe.
    let regions_now: Vec<&strider_lift::cfg::Region> = cfg.regions().collect();
    for region in regions_now.iter().skip(*vn_cache_region_count) {
        for wrapped in region.insns.iter() {
            for vn in wrapped.insn.all_vns() {
                vn_cache.insert(vn);
            }
        }
    }
    *vn_cache_region_count = regions_now.len();
    let mut all_vns: Vec<rsleigh::Vn> = vn_cache.iter().copied().collect();
    all_vns.sort_unstable_by_key(strider_lift::pcode_lift::vn_sort_key);

    // Wrap the IR lift step as `LiftError`.  `analyze_cfg_with`
    // surfaces sleigh decode failures, pcode-lift type errors,
    // unsupported register-aliasing widths, etc. — everything the
    // Python boundary should report as `LiftError` rather than the
    // catch-all `StriderError`.  The typed `UnknownCallOtherError`
    // still flows through unchanged: it's an `anyhow::Error` whose
    // typed root takes precedence over the `LiftError` wrapper at the
    // strider-py boundary (the downcast for `UnknownCallOtherError`
    // runs before the `LiftError` arm).
    let outcome = opts.strider.analyze_cfg_with(
        &cfg,
        crate::AnalyzeOptions {
            all_vns: Some(all_vns),
            per_address_ccs: &opts.per_address_built_ccs,
        },
    ).map_err(|e| {
        // Preserve the typed `UnknownCallOtherError` root if the lift
        // produced one — wrapping it in `LiftError` would hide the
        // typed downcast at the strider-py boundary.
        if e.downcast_ref::<crate::UnknownCallOtherError>().is_some() {
            e
        } else {
            strider_lift::LiftError::wrap(e)
        }
    })?;
    let region_index = RegionIndex::from_handles(&outcome.region_handles);
    let mut graph = outcome.graph;
    let unresolved = outcome.unresolved_branches;

    let pipeline = opts.strider.build_stable_optimizer_pipeline();
    let entry = graph.entry();
    pipeline.run(graph.graph_mut(), entry)?;

    // Harvest the Sleigh handle out of the consumed Cfg so the next
    // iteration can re-use it without re-loading the SLA spec.
    let harvested = cfg.into_sleigh();
    Ok((graph, unresolved, region_index, harvested))
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
    map: &HashMap<PcodeInsnAddr, ResolvedTargets>,
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
        Strider::new(arch, regs, strider_target::CallingConvention::x86_64_systemv())
            .expect("strider")
    }

    fn opts_for_is_tail_call_tests<'a>(
        strider: &'a Strider,
        start_addr: u64,
        fn_max_size: Option<u64>,
        allow_code_before_start_addr: bool,
    ) -> RunOpts<'a> {
        RunOpts {
            strider,
            start_addr: start_addr.into(),
            rom: None,
            fn_max_size,
            allow_code_before_start_addr,
            compact: true,
            per_address_built_ccs: HashMap::new(),
        }
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
        let map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        assert!(edge_set_of(&map).is_empty());
    }

    #[test]
    fn edge_set_of_single_link_register_resolution() {
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(pcode_addr(0x1000), ResolvedTargets::LinkRegister);
        let edges = edge_set_of(&map);
        assert_eq!(edges.len(), 1);
        assert!(edges.contains(&(pcode_addr(0x1000), None)));
    }

    #[test]
    fn edge_set_of_single_resolution_matches_single_edge() {
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(pcode_addr(0x1000), ResolvedTargets::Single(0x2000));
        let edges = edge_set_of(&map);
        let expected: BTreeSet<(PcodeInsnAddr, Option<u64>)> =
            std::iter::once((pcode_addr(0x1000), Some(0x2000))).collect();
        assert_eq!(edges, expected);
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
    }

    #[test]
    fn edge_set_is_order_independent() {
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
        let mut map: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        map.insert(
            pcode_addr(0x1000),
            ResolvedTargets::Multiple(vec![0x2000, 0x2000, 0x2000]),
        );
        assert_eq!(edge_set_of(&map).len(), 1);
    }

    #[test]
    fn is_tail_call_target_below_start_addr_is_tail_call() {
        let strider = make_strider_x86_64();
        let opts = opts_for_is_tail_call_tests(&strider, 0x1000, None, false);
        assert!(is_tail_call(0x0fff, &opts));
        assert!(!is_tail_call(0x1000, &opts));
        assert!(!is_tail_call(0x1001, &opts));
    }

    #[test]
    fn is_tail_call_allow_code_before_start_addr_disables_below_check() {
        let strider = make_strider_x86_64();
        let opts = opts_for_is_tail_call_tests(&strider, 0x1000, None, true);
        assert!(!is_tail_call(0x0fff, &opts));
    }

    #[test]
    fn is_tail_call_above_fn_max_size_is_tail_call() {
        let strider = make_strider_x86_64();
        let opts = opts_for_is_tail_call_tests(&strider, 0x1000, Some(0x100), false);
        // `[start_addr, start_addr + fn_max_size)` is the in-function
        // half-open range: target == end_exclusive is a tail call.
        assert!(is_tail_call(0x1100, &opts));
        assert!(!is_tail_call(0x10ff, &opts));
        assert!(is_tail_call(0x2000, &opts));
    }

    #[test]
    fn is_tail_call_no_fn_max_size_means_above_is_intra_fn() {
        let strider = make_strider_x86_64();
        let opts = opts_for_is_tail_call_tests(&strider, 0x1000, None, false);
        assert!(!is_tail_call(0xffff_ffff_ffff_ffff, &opts));
    }

    #[test]
    fn is_tail_call_fn_max_size_saturates_on_overflow() {
        let strider = make_strider_x86_64();
        let opts = opts_for_is_tail_call_tests(&strider, u64::MAX - 5, Some(0x100), false);
        assert!(is_tail_call(u64::MAX, &opts));
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
