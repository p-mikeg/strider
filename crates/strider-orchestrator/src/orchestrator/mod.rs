//! Top-level analysis driver.
//!
//! [`Strider::analyze`] is the canonical entry point: build the CFG, lift
//! to IR, run the optimiser pipeline, resolve indirect branches via the
//! indirect-resolution fixed-point loop, and return the final IR graph.
//! A [`Strider`] is a per-binary handle (it owns the `Sleigh` / its
//! `MemReader`, a cached `SleighRegs` table, the target arch, and an
//! optional ROM); each `analyze` call lifts one function at a given entry.
//!
//! ## Iteration shape
//!
//! 1. Build the CFG with the current `known_targets` map.
//! 2. Lift the CFG to IR via the cached [`strider_lift::lift::Lifter`].
//! 3. Run the optimiser pipeline (built internally from the per-run
//!    [`strider_opt::OptOptions`]).  Resolution is
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
//! next CFG rebuild.  The CFG builder's `known_targets` path seats
//! `Return` (for `LinkRegister`), `TailCall { target }` (for out-of-range
//! `Single`), `Unconditional` (for in-range `Single`), and `Switch` (for
//! `Multiple`).  No in-place IR edits are applied by the orchestrator.

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow, bail};

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId};
use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_lift::lift::Lifter;
use strider_lift::LiftOptions;
use strider_opt::{OptCtx, OptOptions, ReadOnlyMemory};

use crate::LiftOutcome;

/// Builds the shared [`OptCtx`] for one pipeline run from the
/// orchestrator's borrowed rom slot and the per-run [`OptOptions`].
/// Threaded into every `pipeline.run` site so every iteration of the
/// fixed-point loop sees the same rom image (as the cfg builder) and the
/// same opt configuration (alias precision for every SP-aware pass, plus
/// `call_clobbers_args`).
///
/// The byte order used to decode rom bytes is NOT carried here —
/// `LoadReadOnly` reads it from the function's own `Function::endianness`
/// (the SSoT) at decode time.  `sp_memo` starts empty — the pipeline clears
/// it at every drain.
fn opt_ctx_for_run<'mem>(
    rom: Option<&'mem dyn ReadOnlyMemory>,
    opt_opts: &OptOptions,
) -> OptCtx<'mem> {
    let mut ctx = match rom {
        Some(rom) => OptCtx::with_rom(rom),
        None => OptCtx::empty(),
    };
    ctx.options = opt_opts.clone();
    ctx
}

/// Generic, per-binary analysis handle.
///
/// Holds the reusable [`Lifter`] engine (which owns the target arch, the
/// `Sleigh` context / its `MemReader`, and the cached `SleighRegs` table)
/// plus an optional read-only memory image for `LoadReadOnly`
/// constant-load folding.
///
/// Each [`Strider::analyze`] call lifts one function at a given entry,
/// drives the indirect-branch fixed-point loop, and returns the final IR.
/// The per-function inputs (entry, calling convention, lift options, opt
/// options) are passed per call.
pub struct Strider<R>
where
    R: rsleigh::MemReader,
{
    /// The reusable lift engine (arch + owned Sleigh + cached SleighRegs).
    /// Borrowed `&mut` per rebuild to build + lift the CFG; reused across
    /// every function and every rebuild iteration.
    lifter: Lifter<R>,
    /// Read-only memory image for the optimiser's `LoadReadOnly` pass.
    /// `None` to disable.  Owned for the handle's lifetime; threaded by
    /// `&dyn` reference into the [`OptCtx`] each pipeline run (no `Arc`
    /// sharing — strider runs single-threaded).
    rom: Option<Box<dyn ReadOnlyMemory>>,
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// Construct a `Strider` for `arch` owning `sleigh`, caching the
    /// register table once (via [`Lifter::new`]).
    ///
    /// # Errors
    ///
    /// Returns `Err` if `Sleigh::regs()` fails.
    pub fn new(
        arch: strider_target::SleighArch,
        sleigh: rsleigh::Sleigh<R>,
        rom: Option<Box<dyn ReadOnlyMemory>>,
    ) -> Result<Self> {
        let lifter = Lifter::new(arch, sleigh)
            .map_err(|e| anyhow!("Strider::new: Lifter::new failed: {e:?}"))?;
        Ok(Self { lifter, rom })
    }

    /// Returns the target architecture description.
    #[must_use]
    pub fn arch(&self) -> &strider_target::SleighArch {
        self.lifter.arch()
    }

    /// Returns the cached Sleigh register-name table.
    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lifter.sleigh_regs()
    }

    /// Lift the function at `entry`, optimise it to a fixed point,
    /// resolve its indirect branches, and return the final IR.
    ///
    /// `cc` is the function-default calling convention (already resolved
    /// against this handle's register table).  `lift_opts` supplies the
    /// caller's CFG/lift configuration (`cfg.fn_max_size`,
    /// `cfg.allow_code_before_start_addr`, `per_address_ccs`); its
    /// `cfg.known_targets` seed is ignored — the loop grows its own.
    /// `opt_opts` supplies the optimiser configuration (`alias_mode`,
    /// `call_clobbers_args`, `compact`).
    ///
    /// # Errors
    ///
    /// Returns an error when the iteration cap is hit, when unresolved
    /// branches remain at the fixed point, or any error propagated from
    /// the lift / cfg / opt stages.
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        lift_opts: &LiftOptions,
        opt_opts: &OptOptions,
    ) -> Result<strider_ir::Function> {
        // Seed the single owned working LiftOptions carried across every
        // iteration.  `known_targets` starts empty and GROWS in place;
        // `fn_max_size` / `allow_code_before_start_addr` / `per_address_ccs`
        // are copied from the caller's `lift_opts` once.  The tracked-varnode
        // set is scanned fresh from each rebuilt CFG inside the lifter.  The
        // calling convention `cc` is threaded per lift call (the reused
        // `Lifter` engine does not store it).
        let working = LiftOptions {
            cfg: strider_cfg::CfgOptions {
                fn_max_size: lift_opts.cfg.fn_max_size,
                allow_code_before_start_addr: lift_opts.cfg.allow_code_before_start_addr,
                known_targets: FxHashMap::default(),
            },
            per_address_ccs: lift_opts.per_address_ccs.clone(),
        };

        let mut state = LoopState::new(self, cc, MachineInsnAddr::from(entry), working, opt_opts);
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

/// Lift-time correlation: each deferred `BranchIndirect`'s pcode address
/// paired with the `NodeId` of the `IndirectBranch` placeholder lifted for
/// it.
type UnresolvedAnchors = Vec<(PcodeInsnAddr, strider_ir::node::NodeId)>;

/// Classifier post-pass output: each live `IndirectBranch` placeholder
/// mapped to its classification (`None` = unresolvable this iteration).
type IndirectResolutions = FxHashMap<strider_ir::node::NodeId, Option<ResolvedTargets>>;

/// The fixed-point loop's spanning state for one [`Strider::analyze`]
/// call.
///
/// Borrows the [`Strider`] handle (for its `Sleigh` / arch / rom) and the
/// per-function [`Lifter`] mutably/immutably; owns the working
/// [`LiftOptions`] (whose `known_targets` it grows in place across
/// iterations, avoiding a per-iteration clone) and the loop bookkeeping.
/// `LoopState::finalize` consumes `self` and returns the lifted IR
/// function.
struct LoopState<'a, R>
where
    R: rsleigh::MemReader,
{
    /// The owning handle.  `build_lift` borrows `strider.lifter` mutably
    /// (to build + lift the CFG) and `strider.rom` immutably (for the
    /// `OptCtx`); both fields are disjoint so the split borrow is sound.
    strider: &'a mut Strider<R>,
    /// The function-default calling convention, threaded into each lift
    /// call (the reused `Lifter` engine no longer stores it).
    cc: &'a strider_target::BuiltCallingConvention,
    /// The function entry address (CFG build seed).
    start_addr: MachineInsnAddr,
    /// Per-run optimiser configuration (alias mode, call_clobbers_args,
    /// compact).  Borrowed for the run; read at every pipeline run (via
    /// [`opt_ctx_for_run`]) and at finalize (for `compact`).
    opt_opts: &'a OptOptions,
    /// The single owned working lift options carried across iterations.
    /// `known_targets` is the IR-level indirect-branch resolver accumulator
    /// (grows monotonically in place; see below).  `fn_max_size` /
    /// `allow_code_before_start_addr` / `per_address_ccs` are seeded once
    /// from the caller's `LiftOptions` and never mutated.
    ///
    /// On `known_targets`: per-iteration classifications overlay this map
    /// (so an upgrade like `Single(K1) → Multiple([K1, K2])` overwrites the
    /// entry), but anchors no longer in the per-iteration `unresolved` list
    /// (because a previous Rebuild lowered them to switch edges) MUST stay
    /// — wiping them re-introduces the placeholder on the next rebuild and
    /// the loop diverges.
    working: LiftOptions,
    /// The current optimised IR function.  Initialised to an empty
    /// placeholder by [`LoopState::new`] and overwritten with the real
    /// lift result by [`LoopState::build_initial_iteration`] before any
    /// consumer reads it; the empty placeholder is never observed past
    /// construction.  No `Option` wrapper because the post-init
    /// invariant is "always populated".
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
}

impl<'a, R> LoopState<'a, R>
where
    R: rsleigh::MemReader,
{
    fn new(
        strider: &'a mut Strider<R>,
        cc: &'a strider_target::BuiltCallingConvention,
        start_addr: MachineInsnAddr,
        working: LiftOptions,
        opt_opts: &'a OptOptions,
    ) -> Self {
        Self {
            strider,
            cc,
            start_addr,
            opt_opts,
            working,
            // Empty placeholder; overwritten by `build_initial_iteration`
            // before any consumer reads it.
            function: strider_ir::Function::default(),
            unresolved: Vec::new(),
            resolutions: FxHashMap::default(),
            // Placeholder; overwritten by `build_initial_iteration` once the
            // iteration-0 pending count is known.
            guard: StallGuard::new(0),
        }
    }

    /// Iteration 0: build the CFG, lift, and run the optimiser pipeline.
    fn build_initial_iteration(&mut self) -> Result<()> {
        self.lift_and_seat()?;
        self.guard = StallGuard::new(self.unresolved.len());
        Ok(())
    }

    /// Drive `build_lift` once and seat the resulting graph and
    /// unresolved-branch list onto `self`.  Shared helper between
    /// [`Self::build_initial_iteration`] (initial lift) and
    /// [`Self::rebuild`] (post-Rebuild re-lift).
    fn lift_and_seat(&mut self) -> Result<()> {
        let (function, unresolved, resolutions) = self.build_lift()?;
        self.function = function;
        self.unresolved = unresolved;
        self.resolutions = resolutions;
        Ok(())
    }

    /// Build the CFG, lift to IR, and run the optimiser pipeline.
    /// Returns `(function, unresolved, resolutions)`; the Sleigh stays
    /// owned by `self.strider` across iterations.
    ///
    /// Sequencer: builds the CFG via [`Builder::for_arch`] from the
    /// working [`LiftOptions`] (whose `known_targets` is the current
    /// resolution map), runs the IR lift via [`Lifter::analyze_cfg_with`]
    /// (which scans the rebuilt CFG for its tracked-varnode set), and
    /// finishes with the full optimiser pipeline plus the
    /// [`strider_opt::IndirectBranchClassify`] post-pass, whose
    /// classification output (`OptCtx::indirect_resolutions`) is returned
    /// as the third tuple element.
    fn build_lift(
        &mut self,
    ) -> Result<(strider_ir::Function, UnresolvedAnchors, IndirectResolutions)> {
        // Split the `Strider` borrow: the lifter takes `&mut` (to build +
        // lift the CFG) while the optimiser ctx takes `&rom` (disjoint
        // fields).
        let Strider {
            ref mut lifter,
            ref rom,
        } = *self.strider;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        // Build the CFG, then lift it.  The cfg builder only consults the
        // CFG-shaping knobs (`fn_max_size` / `allow_code_before_start_addr`
        // / `known_targets`) of the working `LiftOptions`; the IR-lift knob
        // (`per_address_ccs`) is read at the `analyze_cfg_with` step below,
        // which also scans the rebuilt CFG for its tracked-varnode set.  No
        // cfg-time resolver is installed: every `BranchIndirect` not yet in
        // `known_targets` is deferred via `UnresolvedIndirectBranch` and
        // resolved at the full-function IR level by [`Self::step`].
        let cfg = lifter.build_cfg(self.start_addr, &self.working.cfg)?;

        let LiftOutcome {
            mut function,
            unresolved_branches: unresolved,
            ..
        } = lifter.analyze_cfg_with(&cfg, self.cc, &self.working)?;

        // The orchestrator's loop pipeline appends the analysis-only
        // `IndirectBranchClassify` post-pass: it runs once on the converged
        // graph, classifies every live `IndirectBranch` placeholder, and
        // writes the results into `ctx.indirect_resolutions`.
        let mut pipeline = strider_opt::default_pipeline();
        pipeline.add_post_pass(strider_opt::IndirectBranchClassify::new());
        let mut ctx = opt_ctx_for_run(rom_ref, self.opt_opts);
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
    /// successful classification in `self.working.cfg.known_targets`, and
    /// decides whether the map grew:
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

        let known_targets = &mut self.working.cfg.known_targets;
        let prev_edge_set = edge_set_of(known_targets);
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
                    known_targets.insert(addr, targets);
                }
                None => {
                    min_unresolved = Some(min_unresolved.map_or(addr, |m| m.min(addr)));
                }
            }
        }
        let grew = edge_set_of(known_targets) != prev_edge_set;

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
        self.lift_and_seat()?;
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
        if self.opt_opts.compact {
            self.function.compact()?;
        }
        Ok(self.function)
    }
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
    use strider_cfg::MachineInsnAddr;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(machine),
            insn_index: 0,
        }
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
