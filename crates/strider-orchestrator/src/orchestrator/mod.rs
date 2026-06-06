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
//! 3. Run the optimiser pipeline
//!    ([`crate::LiftDriver::build_optimizer_pipeline`]).  Resolution is
//!    rebuild-driven (there is no per-iteration index to protect), so a
//!    single pipeline — node-removing passes included — runs every
//!    iteration.
//! 4. For each unresolved anchor, run
//!    [`strider_opt::indirect_branch_resolve::classify_anchor`].
//! 5. Record every successful classification into `known_targets`.
//! 6. If `known_targets` grew → rebuild the CFG with the updated map
//!    (the CFG builder seats `Return` / `TailCall` / switch-edge terminators
//!    from `known_targets` at build time, so every resolved branch is
//!    materialised by the re-lift).
//! 7. If `known_targets` did NOT grow → fixed point.  Any branch still
//!    in `unresolved` is genuinely unresolvable; return `Err`.  Otherwise
//!    the last iteration's fully-optimised IR is the result (finalize only
//!    applies the optional `compact`).
//!
//! ## Iteration cap
//!
//! The cap `2 * pending_at_iter_0 + 4` is a conservative bound on the
//! number of legal classification transitions: every transition strictly
//! grows the `known_targets` map (which is bounded by the number of
//! distinct indirect-branch sites).
//!
//! ## Tail-call and link-register detection
//!
//! `LinkRegister`, `Single(K)` (tail-call or intra-fn), and `Multiple`
//! resolutions are all recorded in `known_targets` and materialised on the
//! next CFG rebuild.  The CFG builder's `with_known_targets` path seats
//! `Return` (for `LinkRegister`), `TailCall { target }` (for out-of-range
//! `Single`), `Unconditional` (for in-range `Single`), and `Switch` (for
//! `Multiple`).  No in-place IR edits are applied by the orchestrator.

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow, bail};

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId};
use strider_lift::cfg::{Builder, Cfg, OptionsBuilder, PcodeInsnAddr, ResolvedTargets};
use strider_opt::{OptCtx, ReadOnlyMemory};

/// Builds the shared [`OptCtx`] for one pipeline run from the
/// orchestrator's borrowed rom slot and the lift driver's alias mode.
/// Threaded into every `pipeline.run` site so every iteration of the
/// fixed-point loop sees the same rom image (as the cfg builder) and the
/// same alias precision (as every SP-aware pass).
///
/// The byte order used to decode rom bytes is NOT carried here —
/// `LoadReadOnly` reads it from the function's own `Function::endianness`
/// (the SSoT) at decode time.  `call_clobbers_args` stays at the default
/// `false`: the orchestrator never enabled the conservative call-shadows-
/// slot reading (its pipelines built `FunctionArgDetect::new()` with no
/// override), so the global default preserves the prior behaviour.
/// `sp_memo` starts empty — the pipeline clears it at every drain.
fn opt_ctx_for_run(
    rom: Option<&dyn ReadOnlyMemory>,
    alias_mode: strider_opt::AliasMode,
) -> OptCtx<'_> {
    let mut ctx = match rom {
        Some(rom) => OptCtx::with_rom(rom),
        None => OptCtx::empty(),
    };
    ctx.alias_mode = alias_mode;
    ctx
}
use crate::LiftOutcome;
use crate::strider::LiftDriver;

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
    /// pass.  `None` to disable.  The orchestrator owns it for the
    /// whole run via `Box<dyn ReadOnlyMemory>` and threads it down by
    /// `&dyn` reference (no `Arc` sharing — strider runs single-threaded).
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
    #[must_use]
    pub fn new() -> Self {
        Self {
            rom: None,
            fn_max_size: None,
            allow_code_before_start_addr: false,
            compact: true,
            per_address_ccs_unbuilt: FxHashMap::default(),
        }
    }

    /// Set the read-only memory image for `LoadReadOnly` folding.  The
    /// orchestrator takes ownership via `Box<dyn ReadOnlyMemory>` and
    /// threads it through each pipeline run by reference (no shared
    /// ownership).
    #[must_use]
    pub fn rom(mut self, rom: Box<dyn ReadOnlyMemory>) -> Self {
        self.rom = Some(rom);
        self
    }

    /// Set the function-size cap in bytes.
    #[must_use]
    pub const fn fn_max_size(mut self, n: u64) -> Self {
        self.fn_max_size = Some(n);
        self
    }

    /// Permit the lifter to follow direct branches to targets below
    /// `start_addr` as intra-function code.
    #[must_use]
    pub const fn allow_code_before_start_addr(mut self) -> Self {
        self.allow_code_before_start_addr = true;
        self
    }

    /// Override the compact-on-finalise flag.
    #[must_use]
    pub const fn compact(mut self, c: bool) -> Self {
        self.compact = c;
        self
    }

    /// Install per-target-address CC overrides (unbuilt presets, resolved
    /// against the Sleigh register table inside [`RunConfig::new`]).
    #[must_use]
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
    /// pass.  `None` to disable.  Owned via `Box<dyn ReadOnlyMemory>`
    /// for the duration of the run; threaded by reference
    /// (`self.rom.as_deref()`) into the [`strider_opt::OptCtx`] each
    /// pipeline run.
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
    /// reachable from `entry` via [`strider_ir::Function::retain_reachable`].  Default
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
                        (*cc)
                            .build(&sleigh_regs)
                            .map(|built| (*addr, built))
                            .map_err(|e| anyhow!("per-address CC at {addr:#x} unresolved: {e:?}"))
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
                        (*cc)
                            .build(&sleigh_regs)
                            .map(|built| (*addr, built))
                            .map_err(|e| anyhow!("per-address CC at {addr:#x} unresolved: {e:?}"))
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
    #[must_use]
    pub fn with_alias_mode(mut self, mode: strider_opt::AliasMode) -> Self {
        self.lift_driver = self.lift_driver.with_alias_mode(mode);
        self
    }

    /// Returns the target architecture description.
    #[must_use]
    pub fn arch(&self) -> &strider_target::SleighArch {
        self.lift_driver.lifter.arch()
    }

    /// Returns the resolved function-default calling convention.
    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        self.lift_driver.calling_convention()
    }

    /// Returns the cached Sleigh register-name table.
    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lift_driver.lifter.sleigh_regs()
    }

    /// Borrow the embedded lift driver — exposed so callers that want
    /// the lift surface (`analyze_cfg`, `build_optimizer_pipeline`,
    /// etc.) without owning a `RunConfig` can route through it.
    #[must_use]
    pub fn lift_driver(&self) -> &LiftDriver {
        &self.lift_driver
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
            Decision::Rebuild => state.rebuild()?,
        }
    }
    bail!("indirect-branch resolver did not converge after {cap} iterations")
}

/// Outcome of one [`LoopState::step`] call.
enum Decision {
    /// `known_targets` did not grow this iteration — no new anchor was
    /// resolved.  Return the last iteration's optimised IR (or an error
    /// if unresolved branches remain).
    FixedPoint,
    /// `known_targets` grew — at least one new anchor was classified.
    /// Rebuild the CFG with the updated map; loop.
    Rebuild,
}

/// Loop-termination safety for the fixed-point loop.
///
/// `cap` is the hard upper bound on total iterations, fixed from the
/// pending-anchor count at iteration 0 (`2 * pending + 4`).  `budget` is
/// retained for the unit-test API and future guard extensions; in the
/// current rebuild-driven design every forward step grows `known_targets`
/// (which is bounded), so the `cap` alone guarantees termination.
///
/// A self-contained value type so the guard invariant can be unit-tested
/// directly without standing up a whole `LoopState`.
struct StallGuard {
    /// Hard iteration cap; see [`StallGuard::new`].
    cap: usize,
    /// Stall-iteration budget; retained for the unit-test API.
    budget: usize,
}

impl StallGuard {
    /// Initialise from the pending-anchor count at iteration 0.  The cap
    /// `2 * pending + 4` bounds the loop because every `Rebuild` grows
    /// `known_targets` by at least one entry (monotone progress, bounded
    /// by the initial anchor count).
    fn new(pending_at_iter_0: usize) -> Self {
        Self {
            cap: 2usize.saturating_mul(pending_at_iter_0).saturating_add(4),
            budget: pending_at_iter_0,
        }
    }

    /// Reset the stall budget after a rebuild (forward progress).
    fn reset_budget(&mut self, pending: usize) {
        self.budget = pending;
    }

    /// Record one iteration's progress against the stall budget.
    ///
    /// Retained for unit tests; not called by the main loop in the
    /// current rebuild-driven design (the `cap` alone terminates the
    /// loop since every `Rebuild` step grows `known_targets`).
    ///
    /// # Errors
    /// Returns `Err` when `!edge_set_changed && unresolved_after >
    /// unresolved_before && self.budget == 0`.
    #[cfg_attr(not(test), allow(dead_code))]
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

/// Lift-time correlation: each deferred `BranchIndirect`'s pcode address
/// paired with the `NodeId` of the `IndirectBranch` placeholder lifted for
/// it.
type UnresolvedAnchors = Vec<(PcodeInsnAddr, strider_ir::node::NodeId)>;

/// Classifier post-pass output: each live `IndirectBranch` placeholder
/// mapped to its classification (`None` = unresolvable this iteration).
type IndirectResolutions = FxHashMap<strider_ir::node::NodeId, Option<ResolvedTargets>>;

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
    /// Lift-time correlation for the current iteration: each deferred
    /// `BranchIndirect`'s pcode address paired with the `NodeId` of the
    /// `IndirectBranch` placeholder lifted for it.  Used to key the
    /// classifier post-pass's node-keyed [`Self::resolutions`] back to
    /// pcode addresses for `known_targets`.
    unresolved: UnresolvedAnchors,
    /// Classifier post-pass output for the current iteration: one entry
    /// per **live** `IndirectBranch` placeholder, paired with its
    /// classification (`None` = still unresolvable this iteration).
    /// Filled from `OptCtx::indirect_resolutions` by [`Self::build_lift`];
    /// drained by [`Self::step`].
    resolutions: IndirectResolutions,
    /// Loop-termination guard: iteration cap (see [`StallGuard`]).
    guard: StallGuard,
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
            resolutions: FxHashMap::default(),
            // Placeholder; overwritten by `build_initial_iteration` once the
            // iteration-0 pending count is known.
            guard: StallGuard::new(0),
            vn_cache: VnCache::default(),
        }
    }

    /// Iteration 0: build the CFG, lift, and run the optimiser pipeline.
    fn build_initial_iteration(&mut self) -> Result<()> {
        self.lift_and_seat("build_initial_iteration")?;
        self.guard = StallGuard::new(self.unresolved.len());
        Ok(())
    }

    /// Drive `build_lift` once and seat the resulting graph and
    /// unresolved-branch list onto `self`.  Shared helper between
    /// [`Self::build_initial_iteration`] (initial lift) and
    /// [`Self::rebuild`] (post-Rebuild re-lift).  `phase` is unused now
    /// that the Sleigh is owned (no take/seat dance) — kept for caller
    /// symmetry / future diagnostics.
    fn lift_and_seat(&mut self, _phase: &'static str) -> Result<()> {
        let (function, unresolved, resolutions) = self.build_lift()?;
        self.function = function;
        self.unresolved = unresolved;
        self.resolutions = resolutions;
        Ok(())
    }

    /// Build the CFG, lift to IR, and run the optimiser pipeline.
    /// Returns `(function, unresolved, resolutions)`; the Sleigh stays
    /// owned by `self` across iterations.
    ///
    /// Sequencer: delegates CFG construction to [`build_cfg`], runs the
    /// IR lift via [`LiftDriver::analyze_cfg_with`], harvests the post-lift
    /// accumulated varnode set via [`VnCache::scan_new_regions`], and
    /// finishes with the full optimiser pipeline plus the
    /// [`strider_opt::IndirectBranchClassify`] post-pass, whose
    /// classification output (`OptCtx::indirect_resolutions`) is returned
    /// as the third tuple element.
    fn build_lift(
        &mut self,
    ) -> Result<(strider_ir::Function, UnresolvedAnchors, IndirectResolutions)> {
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
            fn_max_size,
            allow_code_before_start_addr,
            &self.known_targets,
        )?;

        let all_vns = self.vn_cache.scan_new_regions(&cfg);

        let LiftOutcome {
            mut function,
            unresolved_branches: unresolved,
            ..
        } = lift_driver.analyze_cfg_with(
            &cfg,
            sleigh,
            crate::LiftOptions {
                all_vns: Some(all_vns),
                per_address_ccs: Some(per_address_ccs),
            },
        )?;

        // The orchestrator's loop pipeline appends the analysis-only
        // `IndirectBranchClassify` post-pass: it runs once on the converged
        // graph, classifies every live `IndirectBranch` placeholder, and
        // writes the results into `ctx.indirect_resolutions`.  It is kept
        // off the Python-facing `build_optimizer_pipeline` (added here, in
        // the loop) because classification is an orchestrator concern.
        let mut pipeline = lift_driver.build_optimizer_pipeline();
        pipeline.add_post_pass(strider_opt::IndirectBranchClassify::new());
        let mut ctx = opt_ctx_for_run(rom_ref, lift_driver.alias_mode());
        pipeline.run(&mut function, &mut ctx)?;
        let resolutions = std::mem::take(&mut ctx.indirect_resolutions);

        Ok((function, unresolved, resolutions))
    }

    fn no_unresolved(&self) -> bool {
        self.unresolved.is_empty()
    }

    /// Run one iteration of the loop.
    ///
    /// Drains the classifier post-pass's [`Self::resolutions`] (computed
    /// during the preceding lift on the optimised graph), records every
    /// successful classification in `self.known_targets`, and decides
    /// whether the map grew:
    ///
    /// - If `known_targets` grew → `Decision::Rebuild` (the caller will
    ///   re-lift with the updated map).
    /// - If nothing new was resolved (fixed point) → either
    ///   `Decision::FixedPoint` (no live placeholder remains unclassified)
    ///   or `Err` (a live placeholder is still unresolvable, so the
    ///   indirect branch cannot be recovered).
    ///
    /// A placeholder the optimiser proved unreachable never appears in
    /// `resolutions` (the post-pass walks live nodes only), so a dead
    /// indirect branch neither resolves nor blocks the fixed point.
    fn step(&mut self) -> Result<Decision> {
        // Correlate the post-pass's node-keyed results back to the
        // dispatch pcode addresses recorded at lift time.
        let node_to_addr: FxHashMap<strider_ir::node::NodeId, PcodeInsnAddr> =
            self.unresolved.iter().map(|(addr, node)| (*node, *addr)).collect();

        let prev_edge_set = edge_set_of(&self.known_targets);
        // The resolutions map's iteration order is nondeterministic, so
        // track the lowest unresolved addr explicitly rather than relying
        // on "first seen" — a deterministic choice for the error message.
        let mut min_unresolved: Option<PcodeInsnAddr> = None;
        for (node, resolved) in std::mem::take(&mut self.resolutions) {
            let addr = node_to_addr.get(&node).copied().ok_or_else(|| {
                anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
            })?;
            match resolved {
                Some(targets) => {
                    self.known_targets.insert(addr, targets);
                }
                None => {
                    min_unresolved = Some(min_unresolved.map_or(addr, |m| m.min(addr)));
                }
            }
        }
        let grew = edge_set_of(&self.known_targets) != prev_edge_set;

        if !grew {
            // Fixed point: nothing new resolved.  A live placeholder still
            // classified `None` is genuinely unresolvable.
            if let Some(addr) = min_unresolved {
                return Err(anyhow!(
                    "indirect branch at {addr:?} could not be resolved at fixed point"
                ));
            }
            return Ok(Decision::FixedPoint);
        }
        Ok(Decision::Rebuild)
    }

    /// Rebuild the CFG with the updated `known_targets` map and
    /// re-lift.  Used when the loop chose [`Decision::Rebuild`].
    fn rebuild(&mut self) -> Result<()> {
        self.lift_and_seat("rebuild")?;
        self.guard.reset_budget(self.unresolved.len());
        Ok(())
    }

    /// Consume `self` and return the final graph.
    ///
    /// The full optimizer pipeline (including the node-removing passes)
    /// already ran in the last [`Self::lift_and_seat`], so finalize only
    /// applies the optional [`strider_ir::Function::compact`] before
    /// handing the graph back.
    fn finalize(mut self) -> Result<strider_ir::Function> {
        if self.config.compact {
            self.function.compact()?;
        }
        Ok(self.function)
    }

}

/// Decides whether `target` is a tail call — i.e. lies outside the
/// function's address range `[start_addr, start_addr + fn_max_size)`.
/// Delegates to [`strider_lift::cfg::is_addr_tail_call`] so the cfg-time and orchestrator
/// classifications stay in lockstep.
#[cfg_attr(not(test), allow(dead_code))]
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

/// Build the CFG with the strider's arch + the current `known_targets`
/// resolution map.
///
/// Constructs the `OptionsBuilder` from `fn_max_size` /
/// `allow_code_before_start_addr` and seeds the resolved-target map via
/// [`strider_lift::cfg::Builder::with_known_targets`].  No cfg-time
/// resolver is installed: every `BranchIndirect` that is not yet in
/// `known_targets` is deferred via `UnresolvedIndirectBranch` and
/// resolved at the full-function IR level by [`LoopState::step`].  (The
/// rom is consulted only at the IR level by `LoadReadOnly`, via the
/// optimiser's `OptCtx` — the cfg builder takes no rom.)
fn build_cfg<R>(
    sleigh: &mut rsleigh::Sleigh<R>,
    strider: &LiftDriver,
    start_addr: strider_lift::cfg::MachineInsnAddr,
    fn_max_size: Option<u64>,
    allow_code_before_start_addr: bool,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Result<Cfg>
where
    R: rsleigh::MemReader,
{
    let mut opts_builder = OptionsBuilder::new();
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
    Builder::for_arch(strider.lifter.arch(), sleigh, start_addr.addr, cfg_opts)
        .with_known_targets(known_targets.clone())
        .build()
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
/// [`crate::LiftOutcome::region_exit_controls`].  For each exit:
///
/// 1. Walk backward from the exit's producer via
///    [`strider_ir::walk::region_membership_from_exit`] to collect the
///    region's visualisation membership (control spine, halted at
///    `Region` join nodes, then the data-ancestor closure).
/// 2. Build a `strider_ir::function::dot::FunctionDotDumper` limited to that
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
        let membership =
            strider_ir::walk::region_membership_from_exit(function.graph(), exit_control);

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
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(machine),
            insn_index: 0,
        }
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
        assert_eq!(
            g.budget, 3,
            "budget must stay full across 5 count-stable iterations"
        );
    }

    #[test]
    fn stall_guard_count_decrease_does_not_consume_budget() {
        // The natural progress shape: count strictly decreases.
        let mut g = guard_with_budget(3);
        g.record(false, 3, 4)
            .expect("count-decrease must not error");
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
        assert!(!is_tail_call(
            0xffff_ffff_ffff_ffff,
            0x1000u64.into(),
            None,
            false
        ));
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
