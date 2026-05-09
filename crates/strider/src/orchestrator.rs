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
//! 4. For each unresolved anchor, run [`indirect_resolve::classify_anchor_with_rom_and_sp`].
//! 5. Apply in-place IR edits for terminal classifications:
//!    [`opt::apply_link_register`] for `LinkRegister`,
//!    [`opt::apply_tail_call`] for `Single(K)` where `K` is outside
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

use cfg::{Builder, Cfg, DecodeCache, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use ir::node::{NodeId, NodeOutputId};
use opt::ReadOnlyMemory;

use crate::errors::UnresolvedIndirectBranch;
use crate::indirect_resolve::{
    apply_link_register, apply_tail_call, classify_anchor_with_rom_and_sp,
};
use crate::strider::Strider;
use crate::RegionLiftHandles;

/// Configuration for [`run`].  Held outside the function so callers
/// can construct one and reuse the strider / sleigh / options across
/// iterations without re-paying per-iteration setup costs.
pub struct RunConfig<'a, R>
where
    R: rsleigh::MemReader,
{
    /// The strider — stable across iterations.
    pub strider: &'a Strider,
    /// Function entry address.
    pub start_addr: u64,
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
    /// reachable from `entry` via [`ir::walk::walk_graph`].  Default
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
    /// [`target::CallingConvention::x86_64_all_preserving`] (and the
    /// per-arch siblings).  The user supplies raw addresses; symbol
    /// resolution is the caller's responsibility.
    pub per_address_ccs: HashMap<u64, target::CallingConvention>,
}

/// Internal view of [`RunConfig`] without the Sleigh handle — see
/// [`LoopState`] for why the orchestrator threads the Sleigh
/// separately.
struct RunOpts<'a> {
    strider: &'a Strider,
    start_addr: u64,
    rom: Option<std::sync::Arc<dyn ReadOnlyMemory>>,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    compact: bool,
    /// Pre-resolved per-target-address CC overrides.  See the
    /// [`RunConfig::per_address_ccs`] doc.  Resolved once at
    /// `LoopState::new` so any unresolved register name surfaces
    /// before iteration starts.
    per_address_built_ccs: HashMap<u64, target::BuiltCallingConvention>,
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
        graph: &ir::BuiltFunctionGraph,
        placeholder: NodeId,
    ) -> Option<&ExitVnToValue> {
        let inputs: Vec<_> = graph.graph.node_inputs(placeholder).into_iter().collect();
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
pub fn run<R>(config: RunConfig<'_, R>) -> Result<ir::BuiltFunctionGraph>
where
    R: rsleigh::MemReader,
{
    let mut state = LoopState::new(config)?;
    state.build_iter_0()?;
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
    /// from `RunConfig::sleigh` at construction; consumed by
    /// `Builder::with_endianness` per iteration and harvested back from
    /// the resulting `Cfg::sleigh`.  `None` only momentarily inside
    /// `build_lift_stable`.
    sleigh: Option<rsleigh::Sleigh<R>>,
    /// The current optimised IR graph.
    graph: Option<ir::BuiltFunctionGraph>,
    /// Pending placeholder anchors for the current iteration.
    unresolved: Vec<(PcodeInsnAddr, ir::Value)>,
    /// Pending count at iter 0; sets the cap.
    pending_at_iter_0: usize,
    /// Remaining stall budget.  Decrements each consecutive
    /// in-place-only iteration that didn't reduce `unresolved`; reaching
    /// zero is a misclassifying-resolver bug, surfaced as a typed error.
    stall_budget: usize,
    /// Per-iteration region index, rebuilt by `build_iter_0` /
    /// `rebuild` from the latest `RegionLiftHandles` snapshot.
    region_index: RegionIndex,
    /// Cached link-register / stack-pointer varnodes (stable across
    /// iterations).
    lr_vn: Option<rsleigh::Vn>,
    sp_vn: Option<rsleigh::Vn>,
    /// Decode cache shared across CFG rebuilds.  The Sleigh handle
    /// persists for the whole `run`, so this cache stays valid for
    /// every iteration; threaded into each fresh `cfg::Builder` so
    /// machine-instruction decodes are paid once per address per run.
    decode_cache: DecodeCache,
    // TODO(Task17): remove after incremental indirect-resolve lands —
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
    fn new(config: RunConfig<'a, R>) -> Result<Self> {
        let lr_vn = config.strider.calling_convention().link_register_vn();
        let sp_vn = Some(config.strider.calling_convention().stack_ptr_vn());
        // Pre-resolve per-address CC overrides against the same Sleigh
        // register table the function-default CC was built against.
        let per_address_built_ccs: HashMap<u64, target::BuiltCallingConvention> =
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
    fn build_iter_0(&mut self) -> Result<()> {
        self.lift_and_seat("build_iter_0")?;
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
    /// helper between [`Self::build_iter_0`] (initial lift) and
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
        let (next_known, in_place_edits) = self.classify_and_partition()?;
        self.apply_in_place_edits(&in_place_edits)?;
        let prev_unresolved_len = self.unresolved.len();
        let unresolved_after_edits = self.recompute_unresolved(&in_place_edits);

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

        // Track stall: an in-place-only iteration whose unresolved
        // count *grew* without an edge-set change is a real stall —
        // the resolver is producing more unresolved anchors than it
        // resolves.  Round 9 Ask-8 R2 F7: the previous `>=` form also
        // fired on count-stable iterations (one anchor resolved, one
        // new placeholder materialised), which can be legitimate
        // progress through an anchor-replacement chain.  The
        // `cap = 2 * pending_at_iter_0 + 4` outer bound still
        // terminates count-stable infinite loops; this stall guard
        // catches the strictly-growing pathology earlier.
        if !edge_set_changed && unresolved_after_edits.len() > prev_unresolved_len {
            if self.stall_budget == 0 {
                bail!(
                    "in-place edits stalled: {} unresolved branches after edit (grew from {}), no edge-set growth",
                    unresolved_after_edits.len(),
                    prev_unresolved_len,
                );
            }
            self.stall_budget -= 1;
        }

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
        pipeline.run_on_built(graph)?;
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
    fn finalize(mut self) -> Result<ir::BuiltFunctionGraph> {
        let pipeline = self.opts.strider.build_destructive_optimizer_pipeline();
        let compact = self.opts.compact;
        let graph = self.graph_mut()?;
        pipeline.run_on_built(graph)?;
        if compact {
            graph.compact();
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
        for (addr, anchor_output) in &self.unresolved {
            let resolved_opt = classify_anchor_with_rom_and_sp(
                graph,
                *anchor_output,
                self.lr_vn,
                rom_ref,
                self.sp_vn,
            )?;
            let Some(resolved) = resolved_opt else {
                continue;
            };
            let placeholder_return =
                opt::find_placeholder_return_for_anchor(&graph.graph, *anchor_output);
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
        for nid in graph.graph.preorder(graph.entry) {
            if let ir::node::NodeKind::InitialVar(existing) = graph.graph.node_kind(nid)
                && let Ok([out]) = graph.graph.node_outputs_exact::<1>(nid)
            {
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
    fn recompute_unresolved(
        &mut self,
        in_place_edits: &[(NodeId, ResolvedTargets)],
    ) -> Vec<(PcodeInsnAddr, ir::Value)> {
        let unresolved = std::mem::take(&mut self.unresolved);
        if in_place_edits.is_empty() {
            return unresolved;
        }
        let Some(graph) = self.graph.as_ref() else {
            return Vec::new();
        };
        unresolved
            .into_iter()
            .filter(|(_, anchor)| {
                opt::find_placeholder_return_for_anchor(&graph.graph, *anchor).is_some()
            })
            .collect()
    }

    fn graph_mut(&mut self) -> Result<&mut ir::BuiltFunctionGraph> {
        self.graph
            .as_mut()
            .ok_or_else(|| anyhow!("orchestrator: graph not initialised"))
    }
}

/// Decides whether `target` is a tail call — i.e. lies outside the
/// function's address range `[start_addr, start_addr + fn_max_size)`.
/// Delegates to [`cfg::is_addr_tail_call`] so the cfg-time and orchestrator
/// classifications stay in lockstep.
fn is_tail_call(target: u64, opts: &RunOpts<'_>) -> bool {
    cfg::is_addr_tail_call(
        target,
        opts.start_addr,
        opts.fn_max_size,
        opts.allow_code_before_start_addr,
    )
}

fn apply_in_place_edit(
    graph: &mut ir::BuiltFunctionGraph,
    strider: &Strider,
    region_index: &RegionIndex,
    placeholder: NodeId,
    resolved: &ResolvedTargets,
    per_address_built_ccs: &HashMap<u64, target::BuiltCallingConvention>,
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
            );
            apply_link_register(graph, placeholder, &ctx.ret_val_outputs)?;
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
            );
            let new_return = apply_tail_call(
                graph,
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
                graph.graph.set_call_clobbered_override(call_id, clobber_vars);
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
fn locate_spliced_call(graph: &ir::BuiltFunctionGraph, ret: NodeId) -> Option<NodeId> {
    let inputs: Vec<_> = graph.graph.node_inputs(ret).into_iter().collect();
    let ctrl_in = *inputs.first()?;
    let (producer, _slot) = graph.graph.output_definition(ctrl_in);
    if matches!(graph.graph.node_kind(producer), ir::node::NodeKind::Call) {
        return Some(producer);
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
    graph: &mut ir::BuiltFunctionGraph,
    placeholder: NodeId,
    strider: &Strider,
    region_index: &RegionIndex,
    override_cc: Option<&target::BuiltCallingConvention>,
    initial_var_index: &mut HashMap<rsleigh::Vn, NodeOutputId>,
) -> opt::AnchorCallingContext {
    // When an override is supplied, route arg-passing / ret-val /
    // clobber computation through the override CC instead of the
    // function-default.
    let cc: &target::BuiltCallingConvention = override_cc
        .unwrap_or_else(|| strider.calling_convention());
    let region = region_index.region_for_placeholder(graph, placeholder);
    let mut ctx = opt::AnchorCallingContext::default();

    // `initial_var_index` is built once per orchestrator iteration (in
    // `apply_in_place_edits`) and threaded through.  Per-edit cost is
    // O(arg_count) instead of the previous O(N) arena scan.

    for vn in cc.arg_passing_regs() {
        if let Some(out) = read_or_init_var(graph, region, initial_var_index, *vn) {
            ctx.arg_passing_outputs.push(out);
        }
    }
    // Clobber list: with an override, recompute from the override's
    // callee_saved set against the function's tracked variables (via
    // the shared [`override_clobber_vars`] helper, which is also reused
    // by `apply_in_place_edit` after splicing); without, use the
    // precomputed `BuiltFunctionGraph::call_clobbered` shape.
    let override_clobbers: Vec<rsleigh::Vn>;
    let clobber_iter: Box<dyn Iterator<Item = &rsleigh::Vn>> = if let Some(cc) = override_cc {
        override_clobbers = override_clobber_vars(graph, cc, strider).collect();
        Box::new(override_clobbers.iter())
    } else {
        Box::new(graph.call_clobbered.iter())
    };
    for vn in clobber_iter {
        let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) else {
            continue;
        };
        ctx.clobbered_kinds
            .push(ir::node::NodeOutputKind::OutputType(ty));
    }
    for vn in cc.ret_val_regs() {
        if let Some(out) = read_or_init_var(graph, region, initial_var_index, *vn) {
            ctx.ret_val_outputs.push(out);
        }
    }
    ctx
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
    graph: &'a ir::BuiltFunctionGraph,
    cc: &'a target::BuiltCallingConvention,
    strider: &'a Strider,
) -> impl Iterator<Item = rsleigh::Vn> + 'a {
    let stack_ptr_vn = strider.calling_convention().stack_ptr_vn();
    graph
        .variables
        .values()
        .copied()
        .filter(move |v| !cc.callee_saved_regs().contains(v) && *v != stack_ptr_vn)
}

/// Resolve a varnode to its IR value at the placeholder site.
/// Order: (1) region exit `vn_to_value`, (2) existing `InitialVar(vn)`
/// in the graph, (3) freshly-created `InitialVar(vn)`.  Returns
/// `None` when the varnode's byte size has no matching `NodeOutputType`.
fn read_or_init_var(
    graph: &mut ir::BuiltFunctionGraph,
    region: Option<&ExitVnToValue>,
    initial_var_index: &mut HashMap<rsleigh::Vn, NodeOutputId>,
    vn: rsleigh::Vn,
) -> Option<NodeOutputId> {
    if let Some(r) = region
        && let Some(&out) = r.get(&vn)
    {
        return Some(out);
    }
    if let Some(&out) = initial_var_index.get(&vn) {
        return Some(out);
    }
    let ty: ir::node::NodeOutputType = vn.size.try_into().ok()?;
    let nid = graph.graph.create_node(
        ir::node::NodeKind::InitialVar(vn),
        [],
        [ir::node::NodeOutputKind::OutputType(ty)],
    );
    let [out] = graph.graph.node_outputs_exact::<1>(nid).ok()?;
    initial_var_index.insert(vn, out);
    Some(out)
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
    ir::BuiltFunctionGraph,
    Vec<(PcodeInsnAddr, ir::Value)>,
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
    if let Some(lr) = opts.strider.calling_convention().link_register_vn() {
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
    // arch atomically.  `Builder::with_endianness` would silently default the
    // preset to `X86_64`, which causes arch-specific CallOther dispatch
    // (ARM `swi`, AArch64 `CallHyperVisor`/`CallSecureMonitor`) to be looked
    // up under the wrong preset and silently misclassified or rejected.
    let cfg: Cfg<R> = Builder::for_arch(opts.strider.arch(), sleigh, opts.start_addr, cfg_opts)
        .with_known_targets(known_targets.clone())
        .with_decode_cache(decode_cache.clone())
        .build()?;

    // Vn cache: scan only the regions added since the previous
    // iteration (petgraph's StableDiGraph allocates monotonic
    // NodeIndexes, so `regions().skip(prev_count)` yields exactly
    // the new ones).  At iter 0, scans every region.  Region splits
    // leave the cache slightly conservative — see the field doc on
    // LoopState::vn_cache for why that's safe.
    let regions_now: Vec<&cfg::Region> = cfg.regions().collect();
    for region in regions_now.iter().skip(*vn_cache_region_count) {
        for wrapped in region.insns.iter() {
            for vn in wrapped.insn.all_vns() {
                vn_cache.insert(vn);
            }
        }
    }
    *vn_cache_region_count = regions_now.len();
    let mut all_vns: Vec<rsleigh::Vn> = vn_cache.iter().copied().collect();
    all_vns.sort_unstable_by_key(pcode_lift::vn_sort_key);

    let outcome = opts.strider.analyze_cfg_with(
        &cfg,
        crate::AnalyzeOptions {
            all_vns: Some(all_vns),
            per_address_ccs: &opts.per_address_built_ccs,
        },
    )?;
    let region_index = RegionIndex::from_handles(&outcome.region_handles);
    let mut graph = outcome.graph;
    let unresolved = outcome.unresolved_branches;

    let pipeline = opts.strider.build_stable_optimizer_pipeline();
    pipeline.run_on_built(&mut graph)?;

    // Harvest the Sleigh handle out of the consumed Cfg so the next
    // iteration can re-use it without re-loading the SLA spec.
    let Cfg {
        sleigh: harvested, ..
    } = cfg;
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
    use cfg::MachineInsnAddr;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr { addr: machine },
            insn_index: 0,
        }
    }

    fn make_strider_x86_64() -> Strider {
        let arch = crate::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
        Strider::new(arch, regs, crate::CallingConvention::x86_64_systemv())
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
            start_addr,
            rom: None,
            fn_max_size,
            allow_code_before_start_addr,
            compact: true,
            per_address_built_ccs: HashMap::new(),
        }
    }

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
