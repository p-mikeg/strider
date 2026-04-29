//! Top-level analysis driver.
//!
//! [`run`] is the canonical entry point: build the CFG, lift to IR,
//! run the optimiser pipeline, resolve indirect branches via the
//! tier-2 fixed-point loop, and return the final IR graph.
//!
//! ## Iteration shape
//!
//! 1. Build the CFG with the current `known_targets` map.
//! 2. Lift the CFG to IR via [`Strider::analyze_cfg`].
//! 3. Run the **stable** optimiser subset
//!    ([`Strider::build_stable_optimizer_pipeline`]).
//! 4. For each unresolved anchor, run [`indirect_resolve_tier2::classify_anchor_with_rom_and_sp`].
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
//! edge set) so the loop cannot spin indefinitely on a tier-2
//! soundness bug.
//!
//! ## Tail-call detection
//!
//! A `Single(K)` resolution where `K` lies outside the function
//! address range is treated as a tail call and applied as an in-place
//! edit.  Inside-the-function `Single(K)` requires a CFG rebuild
//! because new code becomes reachable.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

use cfg::{Builder, Cfg, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use ir::node::{NodeId, NodeOutputId};
use opt::ReadOnlyMemory;

use crate::indirect_resolve_tier2::{
    apply_link_register, apply_tail_call, classify_anchor_with_rom_and_sp,
};
use crate::strider::Strider;
use crate::RegionLiftHandles;

/// Configuration for [`run`].  Held outside the function so callers
/// can construct one and reuse the strider / sleigh / options across
/// iterations without re-paying per-iteration setup costs.
pub struct RunConfig<'a, B>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    /// The strider — stable across iterations.
    pub strider: &'a Strider,
    /// Function entry address.
    pub start_addr: u64,
    /// The Sleigh context, owned and threaded through every iteration
    /// of the fixed-point loop.  Re-using one Sleigh across iterations
    /// avoids re-loading the SLA spec on every CFG rebuild.
    pub sleigh: rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>,
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
}

/// Per-iteration index built from a lift's [`RegionLiftHandles`]
/// snapshot.  Maps a region's exit-control `NodeOutputId` to the
/// region's exit `vn_to_value` table — what
/// [`build_anchor_calling_context`] needs to thread ABI varnodes
/// through an in-place edit.
struct RegionIndex {
    by_exit_control: HashMap<NodeOutputId, RegionExitInfo>,
}

struct RegionExitInfo {
    exit_vn_to_value: HashMap<rsleigh::Vn, NodeOutputId>,
}

impl RegionIndex {
    fn from_handles(handles: &[RegionLiftHandles]) -> Self {
        let mut by_exit_control = HashMap::with_capacity(handles.len());
        for h in handles {
            by_exit_control.insert(
                h.exit_control,
                RegionExitInfo {
                    exit_vn_to_value: h.exit_vn_to_value.clone(),
                },
            );
        }
        Self { by_exit_control }
    }

    fn region_for_placeholder(
        &self,
        graph: &ir::BuiltFunctionGraph,
        placeholder: NodeId,
    ) -> Option<&RegionExitInfo> {
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
pub fn run<B>(config: RunConfig<'_, B>) -> Result<ir::BuiltFunctionGraph>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
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
struct LoopState<'a, B>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    opts: RunOpts<'a>,
    /// Accumulator of tier-2 resolutions across iterations.  Replaced
    /// each iteration (an upgrade like `Single(K1) → Multiple([K1, K2])`
    /// is a legitimate classification refinement).
    known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>,
    /// The Sleigh handle we thread through every iteration.  Initialised
    /// from `RunConfig::sleigh` at construction; consumed by
    /// `Builder::with_endianness` per iteration and harvested back from
    /// the resulting `Cfg::sleigh`.  `None` only momentarily inside
    /// `build_lift_stable`.
    sleigh: Option<rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>>,
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
}

impl<'a, B> LoopState<'a, B>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
{
    fn new(config: RunConfig<'a, B>) -> Result<Self> {
        let lr_vn = config.strider.calling_convention().link_register_vn;
        let sp_vn = Some(config.strider.calling_convention().stack_ptr_vn);
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
            opts: RunOpts {
                strider: config.strider,
                start_addr: config.start_addr,
                rom: config.rom,
                fn_max_size: config.fn_max_size,
                allow_code_before_start_addr: config.allow_code_before_start_addr,
            },
        })
    }

    /// Iteration 0: build the CFG, lift, run stable opt, snapshot the
    /// region index.
    fn build_iter_0(&mut self) -> Result<()> {
        let sleigh = self
            .sleigh
            .take()
            .ok_or_else(|| anyhow!("orchestrator: sleigh handle missing at build_iter_0"))?;
        let (graph, unresolved, region_index, sleigh) =
            build_lift_stable(sleigh, &self.opts, &self.known_targets)?;
        self.sleigh = Some(sleigh);
        self.region_index = region_index;
        self.graph = Some(graph);
        self.pending_at_iter_0 = unresolved.len();
        // Allow an in-place-only stall for at most `pending_at_iter_0`
        // iterations: each in-place edit must remove at least one
        // placeholder, so we can't legitimately stall that many times
        // in a row without making progress.
        self.stall_budget = self.pending_at_iter_0;
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
                bail!("indirect branch at {addr:?} could not be resolved at fixed point");
            }
            return Ok(Decision::FixedPoint);
        }

        // Track stall: an in-place-only iteration must strictly
        // reduce the unresolved count, or we've found a fixed point
        // in disguise.  Surface as a typed error so a misclassifying
        // resolver shows up before exhausting the cap.
        if !edge_set_changed && unresolved_after_edits.len() >= prev_unresolved_len {
            if self.stall_budget == 0 {
                bail!(
                    "in-place edits stalled: {} unresolved branches after edit, no edge-set growth",
                    unresolved_after_edits.len()
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
    fn rebuild(&mut self) -> Result<()> {
        let sleigh = self
            .sleigh
            .take()
            .ok_or_else(|| anyhow!("orchestrator: sleigh handle missing at rebuild"))?;
        let (graph, unresolved, region_index, sleigh) =
            build_lift_stable(sleigh, &self.opts, &self.known_targets)?;
        self.sleigh = Some(sleigh);
        self.region_index = region_index;
        self.graph = Some(graph);
        self.unresolved = unresolved;
        Ok(())
    }

    /// Run the destructive subset and consume `self`, returning the
    /// final graph.
    fn finalize(mut self) -> Result<ir::BuiltFunctionGraph> {
        let pipeline = self.opts.strider.build_destructive_optimizer_pipeline();
        let graph = self.graph_mut()?;
        pipeline.run_on_built(graph)?;
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
        let mut next_known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
        let mut in_place_edits: Vec<(NodeId, ResolvedTargets)> = Vec::new();
        for (addr, anchor_output) in &self.unresolved {
            let resolved_opt = classify_anchor_with_rom_and_sp(
                graph,
                *anchor_output,
                self.lr_vn,
                rom_ref,
                self.sp_vn,
            );
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
        let graph = self
            .graph
            .as_mut()
            .ok_or_else(|| anyhow!("orchestrator: graph not initialised"))?;
        for (placeholder, resolved) in in_place_edits {
            apply_in_place_edit(graph, strider, region_index, *placeholder, resolved)?;
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
/// Mirrors `cfg::Builder::is_branch_tail_call_nocheck`.
fn is_tail_call(target: u64, opts: &RunOpts<'_>) -> bool {
    if target < opts.start_addr && !opts.allow_code_before_start_addr {
        return true;
    }
    if let Some(fn_max_size) = opts.fn_max_size {
        // Half-open range: targets at or above `end_exclusive` are
        // tail calls.  `saturating_add` caps at `u64::MAX` so the
        // boundary case `target == u64::MAX` is still classified
        // correctly.
        let end_exclusive = opts.start_addr.saturating_add(fn_max_size);
        if end_exclusive <= target {
            return true;
        }
    }
    false
}

fn apply_in_place_edit(
    graph: &mut ir::BuiltFunctionGraph,
    strider: &Strider,
    region_index: &RegionIndex,
    placeholder: NodeId,
    resolved: &ResolvedTargets,
) -> Result<()> {
    match resolved {
        ResolvedTargets::LinkRegister => {
            let ctx = build_anchor_calling_context(graph, placeholder, strider, region_index);
            apply_link_register(graph, placeholder, &ctx.ret_val_outputs)?;
            Ok(())
        }
        ResolvedTargets::Single(target) => {
            let ctx = build_anchor_calling_context(graph, placeholder, strider, region_index);
            let _new_return = apply_tail_call(
                graph,
                placeholder,
                *target,
                &ctx.arg_passing_outputs,
                &ctx.clobbered_kinds,
                &ctx.ret_val_outputs,
            )?;
            Ok(())
        }
        ResolvedTargets::Multiple(_) => Err(anyhow!(
            "apply_in_place_edit called with ResolvedTargets::Multiple — caller must route via CFG rebuild"
        )),
    }
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
) -> opt::AnchorCallingContext {
    let cc = strider.calling_convention();
    let region = region_index.region_for_placeholder(graph, placeholder);
    let mut ctx = opt::AnchorCallingContext::default();

    // Build a per-call `vn → InitialVar.output` lookup so each
    // `read_or_init_var` is O(1) instead of an arena scan.
    let mut initial_var_index: HashMap<rsleigh::Vn, NodeOutputId> = HashMap::new();
    for nid in graph.graph.all_node_ids() {
        if let ir::node::NodeKind::InitialVar(existing) = graph.graph.node_kind(nid)
            && let Ok([out]) = graph.graph.node_outputs_exact::<1>(nid)
        {
            initial_var_index.insert(*existing, out);
        }
    }

    for vn in &cc.arg_passing_regs {
        if let Some(out) = read_or_init_var(graph, region, &mut initial_var_index, *vn) {
            ctx.arg_passing_outputs.push(out);
        }
    }
    // Emit one clobbered slot per `BuiltFunctionGraph::call_clobbered`
    // entry — the canonical shape `FunctionBuilder::build_call`
    // produces.  Pattern queries index directly into `call_clobbered`
    // to recover varnodes; iterating `cc.callee_saved_regs` here would
    // emit the OPPOSITE set (preserved-across-call regs) with the
    // wrong count.
    for vn in graph.call_clobbered.iter() {
        let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) else {
            continue;
        };
        ctx.clobbered_kinds
            .push(ir::node::NodeOutputKind::OutputType(ty));
    }
    for vn in &cc.ret_val_regs {
        if let Some(out) = read_or_init_var(graph, region, &mut initial_var_index, *vn) {
            ctx.ret_val_outputs.push(out);
        }
    }
    ctx
}

/// Resolve a varnode to its IR value at the placeholder site.
/// Order: (1) region exit `vn_to_value`, (2) existing `InitialVar(vn)`
/// in the graph, (3) freshly-created `InitialVar(vn)`.  Returns
/// `None` when the varnode's byte size has no matching `NodeOutputType`.
fn read_or_init_var(
    graph: &mut ir::BuiltFunctionGraph,
    region: Option<&RegionExitInfo>,
    initial_var_index: &mut HashMap<rsleigh::Vn, NodeOutputId>,
    vn: rsleigh::Vn,
) -> Option<NodeOutputId> {
    if let Some(r) = region
        && let Some(&out) = r.exit_vn_to_value.get(&vn)
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
fn build_lift_stable<B>(
    sleigh: rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>,
    opts: &RunOpts<'_>,
    known_targets: &HashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Result<(
    ir::BuiltFunctionGraph,
    Vec<(PcodeInsnAddr, ir::Value)>,
    RegionIndex,
    rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<B>>,
)>
where
    B: rsleigh::mem_readers::BufMemReaderBackingBuffer,
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

    let arch_endianness = opts.strider.arch().endianness;
    let cfg: Cfg<rsleigh::mem_readers::BufMemReader<B>> =
        Builder::with_endianness(sleigh, opts.start_addr, cfg_opts, arch_endianness)
            .with_known_targets(known_targets.clone())
            .build()?;

    let outcome = opts.strider.analyze_cfg(&cfg)?;
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
/// `LinkRegister` produce equivalent edges (no successor) regardless
/// of any address payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EdgeKind {
    LinkRegister,
    Target(u64),
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
        let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
        let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)
            .expect("probe sleigh")
            .regs()
            .expect("probe regs");
        Strider::new(arch, regs, crate::CallingConvention::x86_64_systemv_abi())
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
