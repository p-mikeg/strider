//! Top-level analysis driver.
//!
//! [`run`] is the canonical entry point: build the CFG, lift to IR,
//! run the optimiser pipeline, resolve indirect branches via the
//! indirect-resolution fixed-point loop, and return the final IR graph.
//!
//! ## Iteration shape
//!
//! 1. Build the CFG with the current `known_targets` map.
//! 2. Lift the CFG to IR via [`crate::LiftDriver::analyze_cfg`].
//! 3. Run the **stable** optimiser subset
//!    ([`crate::LiftDriver::build_stable_optimizer_pipeline`]).
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
//!    ([`crate::LiftDriver::build_destructive_optimizer_pipeline`]) once and
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

use strider_lift::cfg::{Builder, Cfg, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::{NodeId, ValueId};
use crate::opt::{OptCtx, ReadOnlyMemory};

use crate::opt::indirect_branch_resolve::{
    apply_link_register, apply_tail_call, classify_anchor,
};

/// Builds an [`OptCtx`] from the orchestrator's borrowed rom slot and
/// the run's target byte order.  Threaded into every `pipeline.run` site
/// so every iteration of the fixed-point loop sees the same rom image as
/// the cfg builder and decodes its raw bytes with the run's endianness.
fn ctx_from_rom<'mem>(
    rom: Option<&'mem dyn ReadOnlyMemory>,
    endianness: strider_target::Endianness,
) -> OptCtx<'mem> {
    match rom {
        Some(rom) => OptCtx::with_rom_endian(rom, endianness),
        None => OptCtx::with_endian(endianness),
    }
}
use strider_pattern::GraphRewriteCtxExt;
use crate::strider::{LiftDriver, RegionLiftHandles};
use crate::AnalyzeOutcome;

/// Optional knobs for [`RunConfig::new`].  The required arguments (arch,
/// calling convention, sleigh, start address) live on the constructor's
/// positional list; everything else flows through here so the constructor
/// signature stays manageable.
///
/// Use `RunOptions::default()` for the common case ("just analyse this
/// function with no overrides, defaults everywhere"), or the chainable
/// setters below to tweak individual fields.
#[derive(Default)]
pub struct RunOptions {
    /// Read-only memory image for the optimiser's `LoadReadOnly`
    /// pass and the cfg-time indirect-branch resolver.  `None` to
    /// disable.  The orchestrator owns it for the whole run via
    /// `Box<dyn ReadOnlyMemory>` and threads it down by `&dyn`
    /// reference (no `Arc` sharing — strider runs single-threaded).
    pub rom: Option<Box<dyn ReadOnlyMemory>>,
    /// Maximum function size in bytes.  When set, a `Single(K)`
    /// resolution with `K >= start_addr + fn_max_size` is treated as a
    /// tail call.  When `None`, only `K < start_addr` is treated as a
    /// tail call.
    pub fn_max_size: Option<u64>,
    /// When `true`, `Single(K)` with `K < start_addr` is NOT treated
    /// as a tail call — i.e. the orchestrator follows it as an
    /// intra-fn branch.
    pub allow_code_before_start_addr: bool,
    /// Compact the IR arena at finalize.  Default `true` (recommended).
    /// See [`RunConfig::compact`] for the full contract.
    pub compact: bool,
    /// Per-target-address calling-convention overrides.  See
    /// [`RunConfig::per_address_ccs`] for semantics; these are the
    /// unbuilt presets — [`RunConfig::new`] resolves them against the
    /// Sleigh register table at construction.
    pub per_address_ccs_unbuilt: FxHashMap<u64, strider_target::CallingConvention>,
}

impl RunOptions {
    /// Construct with `compact = true` (the recommended default).
    /// Cannot use `#[derive(Default)]` alone because `compact`'s default
    /// must be `true`, not `false`.  Implemented as `#[must_use]` chain-
    /// friendly setters; callers do `RunOptions::new().rom(...).compact(false)`.
    pub fn new() -> Self {
        Self {
            rom: None,
            fn_max_size: None,
            allow_code_before_start_addr: false,
            compact: true,
            per_address_ccs_unbuilt: FxHashMap::default(),
        }
    }

    /// Set the read-only memory image for `LoadReadOnly` folding and
    /// the cfg-time indirect-branch resolver.  The orchestrator takes
    /// ownership via `Box<dyn ReadOnlyMemory>` and threads it through
    /// each pipeline run by reference (no shared ownership).
    pub fn rom(mut self, rom: Box<dyn ReadOnlyMemory>) -> Self {
        self.rom = Some(rom);
        self
    }

    /// Set the function-size cap in bytes.
    pub const fn fn_max_size(mut self, n: u64) -> Self {
        self.fn_max_size = Some(n);
        self
    }

    /// Permit the lifter to follow direct branches to targets below
    /// `start_addr` as intra-function code.
    pub const fn allow_code_before_start_addr(mut self) -> Self {
        self.allow_code_before_start_addr = true;
        self
    }

    /// Override the compact-on-finalise flag.
    pub const fn compact(mut self, c: bool) -> Self {
        self.compact = c;
        self
    }

    /// Install per-target-address CC overrides (unbuilt presets, resolved
    /// against the Sleigh register table inside [`RunConfig::new`]).
    pub fn per_address_ccs_unbuilt(
        mut self,
        m: FxHashMap<u64, strider_target::CallingConvention>,
    ) -> Self {
        self.per_address_ccs_unbuilt = m;
        self
    }
}

/// Configuration for [`run`].  Bundles the stable per-architecture
/// description (arch, calling convention, sleigh-regs cache, alias mode)
/// with the per-run knobs (start address, sleigh handle, ROM, function
/// size cap, code-before-start permission, compact flag, per-address CC
/// overrides) so callers construct one struct and feed it to [`run`].
///
/// All fields are resolved at construction time by [`RunConfig::new`];
/// the per-address CC overrides are pre-built against the Sleigh register
/// table so the loop sees a fully-resolved struct.
pub struct RunConfig<R>
where
    R: rsleigh::MemReader,
{
    /// Stable lift driver: arch + calling convention + sleigh regs +
    /// alias mode.  Embedded here so the orchestrator loop and the
    /// `analyze_cfg` / pipeline-builder helpers all share one source
    /// of truth; the four fields are also surfaced as `pub` accessors
    /// on `RunConfig` for direct inspection.
    lift_driver: LiftDriver,
    /// Function entry address.  Newtype prevents accidental swap with
    /// `fn_max_size` at struct-literal construction sites.  Construct
    /// via `addr.into()` or `strider_lift::cfg::MachineInsnAddr::from(addr)`.
    pub start_addr: strider_lift::cfg::MachineInsnAddr,
    /// The Sleigh context, owned and threaded through every iteration
    /// of the fixed-point loop.  Re-using one Sleigh across iterations
    /// avoids re-loading the SLA spec on every CFG rebuild.
    pub sleigh: rsleigh::Sleigh<R>,
    /// Read-only memory image for the optimiser's `LoadReadOnly`
    /// pass and the cfg-time indirect-branch resolver.  `None` to
    /// disable.  Owned via `Box<dyn ReadOnlyMemory>` for the duration
    /// of the run; threaded by reference (`self.rom.as_deref()`) into
    /// the [`crate::opt::OptCtx`] each pipeline run and into the cfg
    /// builder per rebuild.
    pub rom: Option<Box<dyn ReadOnlyMemory>>,
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
    /// Per-target-address calling-convention overrides, pre-resolved
    /// against the Sleigh register table by [`RunConfig::new`].  When
    /// a `Call` is emitted (either at lift time for a direct call to an
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
    pub per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention>,
}

impl<R> RunConfig<R>
where
    R: rsleigh::MemReader,
{
    /// Build a `RunConfig` from raw inputs.  Resolves the calling
    /// convention and every entry of `options.per_address_ccs_unbuilt`
    /// against the Sleigh register table so the orchestrator sees a
    /// fully-resolved struct.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `Sleigh::regs()` fails, if the function-default
    /// calling convention can't be resolved against the resulting
    /// register table, or if any per-address override CC can't be
    /// resolved.
    pub fn new(
        arch: strider_target::SleighArch,
        calling_convention: strider_target::CallingConvention,
        sleigh: rsleigh::Sleigh<R>,
        start_addr: strider_lift::cfg::MachineInsnAddr,
        options: RunOptions,
    ) -> Result<Self> {
        let sleigh_regs = sleigh
            .regs()
            .map_err(|e| anyhow!("RunConfig::new: Sleigh::regs() failed: {e:?}"))?;
        let lift_driver = LiftDriver::new(arch, sleigh_regs.clone(), calling_convention)?;
        let per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention> =
            if options.per_address_ccs_unbuilt.is_empty() {
                FxHashMap::default()
            } else {
                options
                    .per_address_ccs_unbuilt
                    .iter()
                    .map(|(addr, cc)| {
                        (*cc).build(&sleigh_regs).map(|built| (*addr, built)).map_err(|e| {
                            anyhow!("per-address CC at {addr:#x} unresolved: {e:?}")
                        })
                    })
                    .collect::<Result<_>>()?
            };
        Ok(Self {
            lift_driver,
            start_addr,
            sleigh,
            rom: options.rom,
            fn_max_size: options.fn_max_size,
            allow_code_before_start_addr: options.allow_code_before_start_addr,
            compact: options.compact,
            per_address_ccs,
        })
    }

    /// Build a `RunConfig` from an already-resolved
    /// `BuiltCallingConvention`.  Sister of [`Self::new`] for the
    /// custom-CC path (e.g. CCs constructed from runtime register-name
    /// lists at the Python boundary).
    ///
    /// `options.per_address_ccs_unbuilt` is still resolved against the
    /// Sleigh register table — only the function-default CC is taken
    /// pre-resolved.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `Sleigh::regs()` fails or if any per-address
    /// override CC can't be resolved against the resulting register
    /// table.
    pub fn from_built_cc(
        arch: strider_target::SleighArch,
        calling_convention: strider_target::BuiltCallingConvention,
        sleigh: rsleigh::Sleigh<R>,
        start_addr: strider_lift::cfg::MachineInsnAddr,
        options: RunOptions,
    ) -> Result<Self> {
        let sleigh_regs = sleigh
            .regs()
            .map_err(|e| anyhow!("RunConfig::from_built_cc: Sleigh::regs() failed: {e:?}"))?;
        let lift_driver = LiftDriver::from_built_cc(arch, sleigh_regs.clone(), calling_convention);
        let per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention> =
            if options.per_address_ccs_unbuilt.is_empty() {
                FxHashMap::default()
            } else {
                options
                    .per_address_ccs_unbuilt
                    .iter()
                    .map(|(addr, cc)| {
                        (*cc).build(&sleigh_regs).map(|built| (*addr, built)).map_err(|e| {
                            anyhow!("per-address CC at {addr:#x} unresolved: {e:?}")
                        })
                    })
                    .collect::<Result<_>>()?
            };
        Ok(Self {
            lift_driver,
            start_addr,
            sleigh,
            rom: options.rom,
            fn_max_size: options.fn_max_size,
            allow_code_before_start_addr: options.allow_code_before_start_addr,
            compact: options.compact,
            per_address_ccs,
        })
    }

    /// Override the alias-analysis precision propagated to every
    /// SP-aware pass the pipeline builders construct.
    pub fn with_alias_mode(mut self, mode: crate::opt::AliasMode) -> Self {
        self.lift_driver = self.lift_driver.with_alias_mode(mode);
        self
    }

    /// Returns the target architecture description.
    pub fn arch(&self) -> &strider_target::SleighArch {
        &self.lift_driver.arch
    }

    /// Returns the resolved function-default calling convention.
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        self.lift_driver.calling_convention()
    }

    /// Returns the cached Sleigh register-name table.
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        &self.lift_driver.sleigh_regs
    }

    /// Borrow the embedded lift driver — exposed so callers that want
    /// the lift surface (`analyze_cfg`, `build_optimizer_pipeline`,
    /// etc.) without owning a `RunConfig` can route through it.
    pub fn lift_driver(&self) -> &LiftDriver {
        &self.lift_driver
    }
}

/// A single region's exit `vn_to_value` table: maps each exit varnode
/// (`rsleigh::Vn`) to the `ValueId` producing its value — what
/// [`crate::opt::AnchorCallingContext::for_anchor`] needs to thread
/// ABI varnodes through an in-place edit.
///
/// Owned by value (each `RegionLiftHandles` is consumed once, by
/// `from_handles`, via `into_iter`).
type ExitVnToValue = rustc_hash::FxHashMap<rsleigh::Vn, ValueId>;

/// Per-iteration index built from a lift's [`RegionLiftHandles`]
/// snapshot.  Maps a region's exit-control `ValueId` to that
/// region's [`ExitVnToValue`] table.  Keyed by `ValueId` which
/// impls `EntityRef`, so `FxHashMap` (not `std::HashMap`'s SipHash) is
/// the appropriate entity-keyed map per CLAUDE.md.
struct RegionIndex {
    by_exit_control: rustc_hash::FxHashMap<ValueId, ExitVnToValue>,
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

    fn exit_vars_for_placeholder(
        &self,
        graph: &strider_ir::Graph,
        placeholder: NodeId,
    ) -> Option<&ExitVnToValue> {
        let ctrl_value = graph.nth_input(placeholder, 0)?;
        self.by_exit_control.get(&ctrl_value)
    }
}

/// Drives the iterate-resolve-feed-back loop.
///
/// Consumes the [`RunConfig`] — the loop's `LoopState` takes ownership
/// of every field (including the sleigh) so iteration can mutate the
/// shared state freely.
///
/// # Errors
///
/// Returns an error when the iteration cap is hit, when unresolved
/// branches remain at fixed point, or any error propagated from
/// strider / cfg / opt.
pub fn run<R>(config: RunConfig<R>) -> Result<strider_ir::Function>
where
    R: rsleigh::MemReader,
{
    let mut state = LoopState::new(config);
    state.build_initial_iteration()?;
    if state.no_unresolved() {
        return state.finalize();
    }
    let cap = state.guard.cap;
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

/// Loop-termination safety for the fixed-point loop.
///
/// `cap` is the hard upper bound on total iterations, fixed from the
/// pending-anchor count at iteration 0 (`2 * pending + 4`).  `budget` is
/// the soft allowance for consecutive in-place-only iterations that fail
/// to make progress; it is reset on every rebuild (which is by definition
/// forward progress) and decremented only when an in-place-only iteration
/// strictly grows the unresolved count.
///
/// A self-contained value type so the guard invariant can be unit-tested
/// directly without standing up a whole `LoopState`.
struct StallGuard {
    /// Hard iteration cap; see [`StallGuard::new`].
    cap: usize,
    /// Remaining consecutive-stall allowance.
    budget: usize,
}

impl StallGuard {
    /// Initialise from the pending-anchor count at iteration 0.  The cap
    /// `2 * pending + 4` bounds even count-stable infinite loops; the
    /// budget allows one stall per pending anchor (each in-place edit must
    /// remove at least one placeholder, so we can't legitimately stall
    /// more often than that without progress).
    fn new(pending_at_iter_0: usize) -> Self {
        Self {
            cap: 2usize.saturating_mul(pending_at_iter_0).saturating_add(4),
            budget: pending_at_iter_0,
        }
    }

    /// Reset the stall budget after a rebuild grew the edge set (forward
    /// progress), proportional to what's still pending.
    fn reset_budget(&mut self, pending: usize) {
        self.budget = pending;
    }

    /// Record one iteration's progress.
    ///
    /// Fires `Err` when an in-place-only iteration's unresolved count
    /// **strictly grew** AND the budget is exhausted.  Count-stable
    /// iterations (`unresolved_after == unresolved_before`) do NOT consume
    /// budget: they may represent real progress through an
    /// anchor-replacement chain (one anchor resolved, one new placeholder
    /// materialised); the `cap` still terminates such loops.
    ///
    /// # Errors
    /// Returns `Err` when `!edge_set_changed && unresolved_after >
    /// unresolved_before && self.budget == 0`.
    fn record(
        &mut self,
        edge_set_changed: bool,
        unresolved_after: usize,
        unresolved_before: usize,
    ) -> Result<()> {
        if !edge_set_changed && unresolved_after > unresolved_before {
            if self.budget == 0 {
                bail!(
                    "in-place edits stalled: {} unresolved branches after edit (grew from {}), no edge-set growth",
                    unresolved_after,
                    unresolved_before,
                );
            }
            self.budget -= 1;
        }
        Ok(())
    }
}

/// Cross-rebuild cache of the varnodes seen so far, feeding
/// `analyze_cfg_with`'s `all_vns`.  A pure performance optimisation:
/// dropping it only re-scans regions that were already scanned.
#[derive(Default)]
struct VnCache {
    /// Every varnode seen across all iterations.
    set: rustc_hash::FxHashSet<rsleigh::Vn>,
    /// High-water mark of regions already scanned.  `set` is up-to-date
    /// for the first `region_count` regions; later regions are new and
    /// get scanned and unioned in.
    region_count: usize,
}

impl VnCache {
    /// Union the varnodes from any regions added since the last call into
    /// the cache, then return the sorted set as a `Vec` ready to feed into
    /// `strider.analyze_cfg_with`.
    ///
    /// petgraph's `StableDiGraph` allocates monotonic `NodeIndex`s, so
    /// `regions().skip(region_count)` yields exactly the new ones; at iter 0
    /// the cache is empty and every region is scanned.  Region splits leave
    /// the cache slightly conservative (an over-tracked vn allocates one
    /// extra `InitialVar` and never miscompiles).
    fn scan_new_regions(&mut self, cfg: &Cfg) -> Vec<rsleigh::Vn> {
        for region in cfg.regions().skip(self.region_count) {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    self.set.insert(vn);
                }
            }
        }
        self.region_count = cfg.regions().count();
        let mut all_vns: Vec<rsleigh::Vn> = self.set.iter().copied().collect();
        all_vns.sort_unstable_by_key(strider_lift::pcode_lift::vn_sort_key);
        all_vns
    }
}

/// The fixed-point loop's spanning state.
///
/// Owns the [`RunConfig`] for the whole run — `LoopState::finalize`
/// consumes `self` and returns the lifted IR function.
struct LoopState<R>
where
    R: rsleigh::MemReader,
{
    /// Configuration owned for the duration of the run.  Carries the
    /// stable lift driver, the sleigh handle (borrowed mutably by
    /// `Builder::for_arch` per iteration), the run knobs, and the
    /// pre-resolved per-address CC overrides.
    config: RunConfig<R>,
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
    /// Loop-termination guard: iteration cap + stall budget (see [`StallGuard`]).
    guard: StallGuard,
    /// Per-iteration region index, rebuilt by `build_initial_iteration` /
    /// `rebuild` from the latest `RegionLiftHandles` snapshot.
    region_index: RegionIndex,
    /// Cross-rebuild varnode cache (see [`VnCache`]).
    vn_cache: VnCache,
}

impl<R> LoopState<R>
where
    R: rsleigh::MemReader,
{
    fn new(config: RunConfig<R>) -> Self {
        Self {
            config,
            known_targets: FxHashMap::default(),
            // Empty placeholder; overwritten by `build_initial_iteration`
            // before any consumer reads it.
            function: strider_ir::Function::default(),
            unresolved: Vec::new(),
            // Placeholder; overwritten by `build_initial_iteration` once the
            // iteration-0 pending count is known.
            guard: StallGuard::new(0),
            region_index: RegionIndex {
                by_exit_control: rustc_hash::FxHashMap::default(),
            },
            vn_cache: VnCache::default(),
        }
    }

    /// Iteration 0: build the CFG, lift, run stable opt, snapshot the
    /// region index.
    fn build_initial_iteration(&mut self) -> Result<()> {
        self.lift_and_seat("build_initial_iteration")?;
        self.guard = StallGuard::new(self.unresolved.len());
        Ok(())
    }

    /// Drive `build_lift_stable` once and seat the resulting graph,
    /// region index, and unresolved-branch list onto `self`.  Shared
    /// helper between [`Self::build_initial_iteration`] (initial lift) and
    /// [`Self::rebuild`] (post-Rebuild re-lift).  `phase` is unused now
    /// that the Sleigh is owned (no take/seat dance) — kept for caller
    /// symmetry / future diagnostics.
    fn lift_and_seat(&mut self, _phase: &'static str) -> Result<()> {
        let (function, unresolved, region_index) = self.build_lift_stable()?;
        self.region_index = region_index;
        self.function = function;
        self.unresolved = unresolved;
        Ok(())
    }

    /// Build the CFG, lift to IR, run the stable optimiser subset.
    /// Returns `(graph, unresolved, region_index)`; the Sleigh stays
    /// owned by `self` across iterations.
    ///
    /// Sequencer: delegates CFG construction to [`build_cfg`], runs the
    /// IR lift via [`LiftDriver::analyze_cfg_with`], harvests the post-lift
    /// accumulated varnode set via [`VnCache::scan_new_regions`], and finishes with the stable
    /// optimiser pipeline.  The named helpers carry the per-step
    /// commentary.
    #[allow(clippy::type_complexity)]
    fn build_lift_stable(
        &mut self,
    ) -> Result<(
        strider_ir::Function,
        Vec<(PcodeInsnAddr, strider_ir::Value)>,
        RegionIndex,
    )> {
        // `build_cfg` borrows the Sleigh mutably for its duration; the
        // owned handle on `self.config` stays usable for the IR lift
        // below and for subsequent iterations.  Decompose the borrow
        // so we can pass `&mut sleigh` while still borrowing `&lift_driver`
        // / `&rom` / `&known_targets`.
        let RunConfig {
            ref lift_driver,
            ref mut sleigh,
            start_addr,
            ref rom,
            fn_max_size,
            allow_code_before_start_addr,
            ref per_address_ccs,
            ..
        } = self.config;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();
        let cfg = build_cfg(
            sleigh,
            lift_driver,
            start_addr,
            rom_ref,
            fn_max_size,
            allow_code_before_start_addr,
            &self.known_targets,
        )?;

        let all_vns = self.vn_cache.scan_new_regions(&cfg);

        let AnalyzeOutcome {
            mut function,
            unresolved_branches: unresolved,
            region_handles,
        } = lift_driver.analyze_cfg_with(
            &cfg,
            sleigh,
            crate::AnalyzeOptions {
                all_vns: Some(all_vns),
                per_address_ccs: Some(per_address_ccs),
            },
        )?;
        let region_index = RegionIndex::from_handles(region_handles);

        let pipeline = lift_driver.build_stable_optimizer_pipeline();
        let ctx = ctx_from_rom(rom_ref, lift_driver.arch.endianness());
        pipeline.run(&mut function, &ctx)?;

        Ok((function, unresolved, region_index))
    }

    fn no_unresolved(&self) -> bool {
        self.unresolved.is_empty()
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

        self.guard.record(
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
        let pipeline = self.config.lift_driver.build_stable_optimizer_pipeline();
        let ctx = ctx_from_rom(
            self.config.rom.as_deref(),
            self.config.arch().endianness(),
        );
        pipeline.run(&mut self.function, &ctx)?;
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
        self.guard.reset_budget(self.unresolved.len());
        Ok(())
    }

    /// Run the destructive subset and consume `self`, returning the
    /// final graph.
    fn finalize(mut self) -> Result<strider_ir::Function> {
        let pipeline = self.config.lift_driver.build_destructive_optimizer_pipeline();
        let compact = self.config.compact;
        let ctx = ctx_from_rom(
            self.config.rom.as_deref(),
            self.config.arch().endianness(),
        );
        pipeline.run(&mut self.function, &ctx)?;
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
        let rom_ref: Option<&dyn ReadOnlyMemory> = self.config.rom.as_deref();
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
        let view = strider_pattern::RewriteCtxView::from_built(function)?;
        let known = crate::opt::analyze_known_bits(view)?;
        let cc = self.config.calling_convention();
        let endianness = self.config.arch().endianness();
        for (addr, anchor_value) in &self.unresolved {
            let resolved_opt = classify_anchor(
                view,
                *anchor_value,
                cc.link_register_vn,
                rom_ref,
                endianness,
                Some(cc.stack_vn),
                &known,
            );
            let Some(resolved) = resolved_opt else {
                continue;
            };
            let placeholder_return =
                crate::opt::find_indirect_branch_placeholder(function.graph(), *anchor_value);
            let can_inplace = match (&resolved, placeholder_return) {
                (ResolvedTargets::LinkRegister, Some(_)) => true,
                (ResolvedTargets::Single(target), Some(_)) => is_tail_call(
                    *target,
                    self.config.start_addr,
                    self.config.fn_max_size,
                    self.config.allow_code_before_start_addr,
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
        let region_index = &self.region_index;
        let per_address_built_ccs = &self.config.per_address_ccs;
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
                crate::opt::find_indirect_branch_placeholder(self.function.graph(), *anchor).is_some()
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
                region_index,
                None,
            )?;
            let _new_return = function.with_rewrite_ctx(|rctx| {
                apply_link_register(rctx, placeholder, &ctx.ret_val_values)
            })?;
            Ok(())
        }
        ResolvedTargets::Single(target) => {
            let override_cc = per_address_built_ccs.get(target);
            let ctx = crate::opt::AnchorCallingContext::for_anchor(
                function,
                placeholder,
                region_index,
                override_cc,
            )?;
            // Memory-preserving CCs (the override's flag, or the function's
            // stored default when no override is in play) suppress the
            // spliced Call's memory clobber so LoadReadOnly / LoadForward
            // chains stay intact across the tail call.
            let preserves_memory = override_cc.map_or_else(
                || function.default_cc().preserves_memory,
                |cc| cc.preserves_memory,
            );
            let sp_value = ctx.sp_value.ok_or_else(|| {
                anyhow!("apply_in_place_edit: AnchorCallingContext is missing the SP anchor value")
            })?;
            let new_return = function.with_rewrite_ctx(|rctx| {
                apply_tail_call(
                    rctx,
                    placeholder,
                    *target,
                    sp_value,
                    &ctx.arg_passing_values,
                    &ctx.ret_val_kinds,
                    &ctx.clobbered_kinds,
                    &ctx.ret_val_values,
                    preserves_memory,
                )
            })?;
            // Tag each spliced Call ret-val + clobber output value with
            // the register it represents (`value_vn`), matching
            // `FunctionBuilder::build_call` so pattern queries recover the
            // right varnode per slot.  The spliced node is the
            // freshly-created Call adjacent to `new_return`'s ctrl
            // predecessor; its outputs are `[Control, Memory] ++ ret_vals
            // ++ clobbers`, so the ordered `ret_val_vns ++ clobber_vns`
            // (both from the same `for_anchor` projection) line up with the
            // outputs past slot 2.  For an override Call we additionally
            // record the CC (subsuming the stack-arg offsets) so the
            // validator checks arity against the tagged outputs.
            if let Some(call_id) = locate_spliced_call(function.graph(), new_return) {
                let tagged_outputs: Vec<ValueId> =
                    function.node_outputs(call_id).iter().copied().skip(2).collect();
                let tag_vns = ctx.ret_val_vns.iter().chain(ctx.clobber_vns.iter());
                for (value, vn) in core::iter::zip(&tagged_outputs, tag_vns) {
                    function.set_clobbered_vn(*value, *vn);
                }
                if let Some(cc) = override_cc {
                    function.set_call_cc(call_id, cc.clone());
                }
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
    let ctrl_value = graph.nth_input(ret, 0)?;
    let (producer, _slot) = graph.value_definition(ctrl_value);
    if matches!(graph.node_kind(producer), strider_ir::node::NodeKind::Call) {
        return Some(producer);
    }
    // Region bridge: walk the Region's first control input
    // and check if THAT producer is a Call.  Mirrors the splice shape
    // when `apply_tail_call`'s freshly-spliced Call feeds an existing
    // Region that the new Return then consumes.
    if matches!(graph.node_kind(producer), strider_ir::node::NodeKind::Region) {
        for cs_in in graph.node_inputs(producer) {
            let (cs_producer, _) = graph.value_definition(cs_in);
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
    /// The `clobbered_kinds` slot mirrors the function's derived
    /// `call_clobbered_regs()` so the resulting Call node's outputs match
    /// the canonical `FunctionBuilder::build_call`-shape.
    ///
    /// `override_cc = Some(cc)` routes arg-passing / ret-val / clobber
    /// computation through `cc` (the per-target-address override — the
    /// callee's ABI for this tail call); `None` uses the function's stored
    /// default convention ([`strider_ir::Function::default_cc`]).
    fn for_anchor(
        function: &mut strider_ir::Function,
        placeholder: NodeId,
        region_index: &RegionIndex,
        override_cc: Option<&strider_target::BuiltCallingConvention>,
    ) -> Result<Self> {
        // The effective convention: the per-address override when present,
        // else the function's stored default CC (the single SSoT — the same
        // convention the function was built under).  Cloned so the
        // `read_or_init_var` passes below are free to borrow `function`
        // mutably while we still hold the convention.
        let effective_cc: strider_target::BuiltCallingConvention =
            override_cc.cloned().unwrap_or_else(|| function.default_cc().clone());
        let region = region_index.exit_vars_for_placeholder(function.graph(), placeholder);
        let mut ctx = Self::default();

        // Each `read_or_init_var` call is O(1) against the function's
        // maintained `Graph::initial_var_for` index — no per-iteration
        // arena scan, no per-edit threading.

        // Args: ABI register-arg order via the canonical
        // `PositionalArgLayout::register_args`, read at the dispatch site.
        let layout = strider_target::PositionalArgLayout::from_convention(&effective_cc);
        for (_index, vn) in layout.register_args() {
            // surface unsupported reg sizes as Err instead
            // of silently dropping the slot (which under-models the Call
            // and can cause downstream pattern queries to miss args).
            let value = read_or_init_var(function, region, vn)?;
            ctx.arg_passing_values.push(value);
        }
        // Read the stack-pointer value at the dispatch site so the spliced
        // Call carries its SP input anchor (slot [3], ahead of the args) —
        // mirroring `FunctionBuilder::build_call`.
        ctx.sp_value = Some(read_or_init_var(function, region, effective_cc.stack_vn)?);

        // Ret-val + clobber OUTPUT groups, derived from the effective CC
        // over the function's tracked varnodes via the SAME accessors
        // `FunctionBuilder::build_call` uses.  This makes a spliced Call
        // structurally identical to a naturally-lifted one: the ret-val
        // regs go in the ret-val group and are EXCLUDED from the clobber
        // group (`call_clobbered_for` filters them out), so an override
        // tail call no longer double-counts them across both groups.
        for vn in function.call_ret_vals_for(&effective_cc) {
            let ty = vn_size_to_node_output_type(&vn)?;
            ctx.ret_val_kinds.push(strider_ir::node::ValueKind::Typed(ty));
            ctx.ret_val_vns.push(vn);
        }
        for vn in function.call_clobbered_for(&effective_cc) {
            // surface unsupported clobber-reg sizes as Err rather than
            // silently defaulting — a size we don't know how to lower
            // would otherwise produce a malformed Call output kind.
            let ty = vn_size_to_node_output_type(&vn)?;
            ctx.clobbered_kinds.push(strider_ir::node::ValueKind::Typed(ty));
            ctx.clobber_vns.push(vn);
        }
        // Raw declared ret-val list fed to the spliced Return — BOTH integer
        // and float regs at declared width, matching the naturally-lifted
        // Return's arity (otherwise AArch64 q0/q1, x86_64 XMM0/XMM1, MIPS
        // f0/f2, PPC f1/f2, ARM d0/d1 slots silently vanish).  Distinct from
        // the tracked-filtered `ret_val_kinds` Call-output group above.
        for vn in effective_cc
            .ret_val_regs
            .iter()
            .chain(effective_cc.ret_val_regs_float.iter())
        {
            let value = read_or_init_var(function, region, *vn)?;
            ctx.ret_val_values.push(value);
        }
        Ok(ctx)
    }
}

/// Map a varnode's byte width to the matching [`strider_ir::node::ValueType`].
///
/// Used by the orchestrator's anchor-calling-context plumbing
/// ([`crate::opt::AnchorCallingContext::for_anchor`] for clobber outputs,
/// `read_or_init_var` for freshly-created `InitialVar` nodes) to surface
/// unsupported sizes as a typed error rather than silently dropping the
/// slot.  Every supported CC preset uses sizes ∈ {1, 2, 4, 8, 10, 16,
/// 32, 64} which all map cleanly; the Err arm exists so a future CC
/// addition with an exotic size surfaces the gap immediately.
fn vn_size_to_node_output_type(vn: &rsleigh::Vn) -> Result<strider_ir::node::ValueType> {
    strider_ir::node::ValueType::int_for_byte_size(vn.size).map_err(|_| {
        anyhow::anyhow!(
            "varnode size {} has no ValueType — calling-convention \
             register {:?} cannot be modelled (supported sizes are 1, 2, 4, \
             8, 10, 16, 32, 64 bytes)",
            vn.size,
            vn,
        )
    })
}

/// Resolve a varnode to its IR value at the placeholder site.
/// Order: (1) region exit `vn_to_value`, (2) existing `InitialVar(vn)`
/// in the graph, (3) freshly-created `InitialVar(vn)`.
///
/// returns an error (instead of silently dropping the
/// varnode) when its byte size has no matching `ValueType`.  In
/// practice every supported CC preset uses sizes ∈ {1, 2, 4, 8, 10,
/// 16, 32, 64} which all map cleanly; the Err arm exists so a future
/// CC addition with an exotic size surfaces the gap immediately
/// instead of producing a Call node with under-modelled inputs.
///
/// # Errors
///
/// Returns `Err` if `vn.size` doesn't map to a `ValueType` or
/// if the freshly-created `InitialVar` doesn't have exactly one
/// output (the `node_signature` invariant guarantees this; the error
/// path exists only for defensive completeness).
fn read_or_init_var(
    function: &mut strider_ir::Function,
    region: Option<&ExitVnToValue>,
    vn: rsleigh::Vn,
) -> Result<ValueId> {
    if let Some(r) = region
        && let Some(&value) = r.get(&vn)
    {
        return Ok(value);
    }
    // Consult the maintained `InitialVar` index.  Skip detached zombies
    // by validating that the registered node's single output still has
    // live uses — a zero-use entry indicates the index points at a
    // detached node, so we fall through and mint a fresh `InitialVar`
    // instead of resurrecting the zombie.
    if let Some(nid) = function.initial_var_for(vn) {
        let [value] = function.node_outputs_exact::<1>(nid).map_err(|e| {
            anyhow!(
                "read_or_init_var: InitialVar({vn:?}) at {nid:?} has wrong output \
                 arity (expected 1): {e}"
            )
        })?;
        if function.graph().value_uses(value).next().is_some() {
            return Ok(value);
        }
    }
    let ty = vn_size_to_node_output_type(&vn)?;
    let nid = function.graph_mut().create_node(
        strider_ir::node::NodeKind::InitialVar(vn),
        [],
        [strider_ir::node::ValueKind::Typed(ty)],
    );
    let [value] = function
        .node_outputs_exact::<1>(nid)
        .expect("freshly created InitialVar has 1 output per node signature");
    function.register_initial_var(vn, nid);
    Ok(value)
}


/// Build the CFG with the strider's arch + the current `known_targets`
/// resolution map.
///
/// Constructs the `OptionsBuilder` from link-register /
/// `fn_max_size` / `allow_code_before_start_addr`, threads the borrowed
/// `rom` through [`strider_lift::cfg::Builder::with_read_only_memory`],
/// and installs the strider-analyze mini-IR indirect-branch resolver.
#[allow(clippy::too_many_arguments)]
fn build_cfg<R>(
    sleigh: &mut rsleigh::Sleigh<R>,
    strider: &LiftDriver,
    start_addr: strider_lift::cfg::MachineInsnAddr,
    rom: Option<&dyn ReadOnlyMemory>,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Result<Cfg>
where
    R: rsleigh::MemReader,
{
    let mut opts_builder = OptionsBuilder::new();
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
    let resolver: strider_lift::cfg::IndirectResolverFn<R> = Box::new(
        |insns, target_vn, sleigh, lr_vn, rom, endianness| {
            crate::indirect_resolver::resolve_indirect_target(
                insns, target_vn, sleigh, lr_vn, rom, endianness,
            )
        },
    );
    let mut builder = Builder::for_arch(&strider.arch, sleigh, start_addr.addr, cfg_opts)
        .with_known_targets(known_targets.clone())
        .with_indirect_resolver(resolver);
    if let Some(rom) = rom {
        builder = builder.with_read_only_memory(rom);
    }
    builder.build()
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

/// Renders `function` filtered to `members` as a dark-themed HTML
/// viewer at `path`.  Shared tail of [`dump_per_region`] and
/// [`dump_neighborhood`]: build a `FunctionDotDumper` limited to
/// `members`, then dump it via `dot::GraphDot`.  `ctx` prefixes the
/// write-failure error message (the caller's function name).
///
/// # Errors
///
/// Returns an error if [`strider_ir::Function::dot_dumper`] fails (graph
/// not built) or if the HTML write to `path` fails.
fn render_filtered_html<R>(
    function: &strider_ir::Function,
    sleigh: &rsleigh::Sleigh<R>,
    members: strider_ir::walk::NodeIdSet,
    path: &std::path::Path,
    ctx: &str,
) -> Result<()>
where
    R: rsleigh::MemReader,
{
    let dumper = function.dot_dumper(sleigh)?.with_node_filter(members);
    ::dot::GraphDot::new(dumper, ::dot::DotStyle::dark())
        .dump_as_html(path)
        .map_err(|e| anyhow!("{ctx}: write {} failed: {e}", path.display()))
}

/// Renders one HTML viewer per region into `out_dir`.
///
/// `exit_controls` names each region by the `ValueId` that its
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
/// Returns an error if [`strider_ir::Function::dot_dumper`] fails (graph
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
    I: IntoIterator<Item = ValueId>,
{
    if function.graph().generation() != lift_generation {
        return Err(anyhow!(
            "dump_per_region: function generation {} does not match lift snapshot {}; \
             the function was compacted after lift and exit_controls are stale",
            function.graph().generation(),
            lift_generation,
        ));
    }
    for (idx, exit_control) in exit_controls.into_iter().enumerate() {
        // Construct a fresh dumper per region via the public
        // `Graph::dot_dumper` + `with_node_filter` chain.  The dumper
        // borrows from `function` / `sleigh`, so we can't reuse one across
        // iterations (each `with_node_filter` consumes the value).
        let membership = strider_ir::walk::region_membership_from_exit(function.graph(), exit_control);

        let producer = function.producer(exit_control);
        // Include `idx` unconditionally: two regions whose producers
        // share a first asm-fingerprint would otherwise collide via
        // `std::fs::write` (silent overwrite).
        let addr_part: String = function
            .asm_fingerprint(producer)
            .first()
            .map_or_else(|| "nofp".to_string(), |a| format!("{a:016x}"));
        let path = out_dir.join(format!("region_{idx}_{addr_part}.html"));
        render_filtered_html(function, sleigh, membership, &path, "dump_per_region")?;
    }
    Ok(())
}

/// Writes an HTML viewer for the subgraph within `depth` hops of
/// `anchor` (forward + backward) to `out_path`.
///
/// Uses [`strider_ir::walk::collect_neighborhood`] to build the visible
/// node set and renders via [`strider_ir::Function::dot_dumper`]'s
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
    if !function.graph().has_node(anchor) {
        return Err(anyhow!(
            "dump_neighborhood: anchor {anchor:?} is not a live node in this function \
             (stale id from a pre-compaction snapshot, or a foreign id)",
        ));
    }
    let visible = strider_ir::walk::collect_neighborhood(function.graph(), anchor, depth);
    render_filtered_html(function, sleigh, visible, out_path, "dump_neighborhood")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_lift::cfg::MachineInsnAddr;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr { machine_addr: MachineInsnAddr::from(machine), insn_index: 0 }
    }

    fn make_strider_x86_64() -> LiftDriver {
        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .expect("x86_64_systemv preset must be registered");
        LiftDriver::new(arch, regs, cc).expect("strider")
    }


    // ── StallGuard tests ──────────────────────────────────────────
    //
    // These pin the stall-guard invariant (the `>=` → `>` fix) by
    // exercising `StallGuard::record` directly, plus the cap formula.
    // Each case names the relevant fixed-point-loop scenario.

    /// A guard with the given budget and an irrelevant cap, for the
    /// `record` cases (which never read `cap`).
    fn guard_with_budget(budget: usize) -> StallGuard {
        StallGuard { cap: 0, budget }
    }

    #[test]
    fn stall_guard_new_sets_cap_and_budget() {
        let g = StallGuard::new(3);
        assert_eq!(g.cap, 10, "cap = 2 * pending + 4");
        assert_eq!(g.budget, 3, "budget seeded from pending");
        let z = StallGuard::new(0);
        assert_eq!(z.cap, 4);
        assert_eq!(z.budget, 0);
    }

    #[test]
    fn stall_guard_no_change_in_count_does_not_consume_budget() {
        // regression: a count-stable in-place-only iteration (one anchor
        // resolved, one new placeholder materialised) is legitimate
        // progress and must NOT consume budget.  Pre-fix (`>=`) ate one
        // budget per stable iteration; post-fix (`>`) it stays full.
        let mut g = guard_with_budget(3);
        for _ in 0..5 {
            g.record(/* edge_set_changed */ false, 4, 4)
                .expect("count-stable iteration must not error");
        }
        assert_eq!(g.budget, 3, "budget must stay full across 5 count-stable iterations");
    }

    #[test]
    fn stall_guard_count_decrease_does_not_consume_budget() {
        // The natural progress shape: count strictly decreases.
        let mut g = guard_with_budget(3);
        g.record(false, 3, 4).expect("count-decrease must not error");
        assert_eq!(g.budget, 3);
    }

    #[test]
    fn stall_guard_count_growth_consumes_budget() {
        // Strictly-growing count (resolver producing more anchors than it
        // resolves) is the real stall pathology.  Each growth step
        // decrements budget; reaching zero raises Err.
        let mut g = guard_with_budget(2);
        g.record(false, 5, 4).expect("first growth ok"); // 4 → 5, budget 2 → 1
        assert_eq!(g.budget, 1);
        g.record(false, 6, 5).expect("second growth ok"); // 5 → 6, budget 1 → 0
        assert_eq!(g.budget, 0);
        let err = g
            .record(false, 7, 6) // 6 → 7, budget 0 → bail
            .expect_err("third growth must surface the stall");
        assert!(
            err.to_string().contains("in-place edits stalled"),
            "got: {err}"
        );
    }

    #[test]
    fn stall_guard_edge_set_change_skips_check() {
        // When edge_set_changed (Rebuild path), the stall guard is
        // entirely skipped.  Budget stays untouched even on growth.
        let mut g = guard_with_budget(1);
        g.record(/* edge_set_changed */ true, 100, 1)
            .expect("rebuild path skips stall check");
        assert_eq!(g.budget, 1, "edge-set change must not consume budget");
    }

    #[test]
    fn stall_guard_zero_budget_with_no_growth_is_ok() {
        // Budget 0 + no growth = no stall fires.  Documents that
        // exhausted budget plus benign progress remains progress.
        let mut g = guard_with_budget(0);
        g.record(false, 4, 4).expect("count-stable + 0-budget ok");
        g.record(false, 3, 4).expect("count-decrease + 0-budget ok");
        g.record(true, 100, 4).expect("edge-change + 0-budget ok");
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
