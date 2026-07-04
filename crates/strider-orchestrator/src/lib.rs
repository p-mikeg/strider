#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Top-level analysis driver.
//!
//! [`Strider::analyze`] is the canonical entry point: build the CFG, lift
//! to IR, run the optimiser pipeline, resolve indirect branches, and
//! return the final IR graph plus any sites that stayed unresolved.
//! A [`Strider`] is a per-binary handle (it owns the `Sleigh` / its
//! `MemReader`, a cached `SleighRegs` table, the target arch, and an
//! optional ROM); each `analyze` call lifts one function at a given entry.
//!
//! ## Iteration shape
//!
//! A plain bounded loop (see [`Strider::analyze`]):
//!
//! 1. Build the CFG with the current `known_targets` map, lift to IR via
//!    the cached [`strider_lift::lift::Lifter`], and run the optimiser
//!    pipeline (a single pipeline — node-removing passes included — runs
//!    every iteration; resolution is rebuild-driven, no per-iteration index
//!    to protect) plus the [`strider_opt::IndirectBranchClassify`]
//!    post-pass.
//! 2. If no `IndirectBranch` placeholder was deferred, stop — fully
//!    resolved.
//! 3. Otherwise fold every successful classification into `known_targets`
//!    (see `apply_resolutions`). If nothing new resolved, stop — the
//!    remaining branches are unresolvable. Else re-lift with the grown map
//!    (the CFG builder seats `Return` / `TailCall` / switch-edge
//!    terminators from `known_targets` at build time).
//!
//! Unresolvable branches are **not** an error: [`Strider::analyze`] returns
//! an [`AnalyzeResult`] whose `unresolved_indirect_branches` lists the
//! sites that are still *live-and-unclassified* in the final function
//! (their placeholders remain in `function`).  A site is **dropped** from
//! that report when the optimizer proved its placeholder dead, or when the
//! loop already folded a classification for it (even one the cfg layer
//! could not seat, e.g. an out-of-range `Multiple` table).
//!
//! `MAX_RESOLUTION_ITERATIONS` is only a backstop — step 3's "nothing new
//! resolved" check is the real terminator.  Convergence does **not** rely
//! on monotone growth of `known_targets` (`apply_resolutions` *overwrites*
//! entries it folds): instead, each site's classification is folded in at
//! most once (`apply_resolutions` skips a site already present in
//! `known_targets`), so a classifier whose re-lifted graph keeps reporting
//! a *different* target set for an un-seatable site cannot churn the loop.
//! Falling through the cap is treated as a non-convergence bug (a
//! `debug_assert!` fires), not a silent stale return.
//!
//! ## Tail-call and link-register detection
//!
//! `LinkRegister`, `Single(K)` (tail-call or intra-fn), and `Multiple`
//! resolutions are all recorded in `known_targets` and materialised on the
//! next CFG rebuild.  The CFG builder's `known_targets` path seats
//! `Return` (for `LinkRegister`), `TailCall { target }` (for out-of-range
//! `Single`), `Unconditional` (for in-range `Single`), and `Switch` (for
//! `Multiple`).  No in-place IR edits are applied by the orchestrator.
//!
//! The optimization passes live in the [`strider_opt`] crate, re-exported
//! here as [`opt`]; the lift engine ([`Lifter`]) and its [`LiftOptions`] /
//! [`LiftOutcome`] types are re-exported from `strider-lift`.

/// The optimization-pass crate, re-exported so downstream consumers can reach
/// passes via `strider_orchestrator::opt::…` alongside the orchestration API.
pub use strider_opt as opt;

pub use strider_lift::LiftOptions;
/// The CFG→IR lift engine and its option / outcome types, re-exported from
/// `strider-lift` so downstream consumers (the Python bindings, tests) reach
/// them via `strider_orchestrator::…` without a direct `strider-lift` dep.
pub use strider_lift::lift::{LiftOutcome, Lifter};

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_opt::{OptCtx, OptOptions, ReadOnlyMemory};

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

    /// Returns the cached Sleigh register-name table.
    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lifter.sleigh_regs()
    }

    /// Returns the owned `Sleigh`, for callers that need to resolve
    /// register names (e.g. dot rendering) through the same instance
    /// `analyze`/`build_cfg` drive.
    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        self.lifter.sleigh()
    }

    /// Build the CFG for `entry` only — no lift, no optimisation, no
    /// indirect-branch resolution.  Reuses the same owned `Lifter` (and
    /// its `Sleigh`) that [`Strider::analyze`] drives, so callers that
    /// want a structural-only preview or a snapshot for dot rendering
    /// don't need a second handle.
    ///
    /// # Errors
    ///
    /// Propagates CFG-build failures (see [`Lifter::build_cfg`]).
    pub fn build_cfg(
        &mut self,
        entry: u64,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        // Standalone structural build: no per-address CCs (they are a lift-time
        // ABI concept threaded only through `analyze`/`build_lift`).
        self.lifter.build_cfg(
            MachineInsnAddr::from(entry),
            cfg_opts,
            &rustc_hash::FxHashMap::default(),
        )
    }

    /// Lift the function at `entry`, optimise it, resolve its indirect
    /// branches, and return the final IR plus any indirect-branch sites
    /// that could not be resolved.
    ///
    /// The algorithm is a plain bounded loop: lift the CFG → IR, optimise,
    /// and if any indirect branch was deferred, classify it (the
    /// `IndirectBranchClassify` post-pass), fold the new targets into
    /// `known_targets`, and re-lift.  It stops as soon as either no
    /// indirect branch remains deferred (full resolution) or an iteration
    /// resolves nothing new (the remaining branches are unresolvable).
    /// Unresolvable branches are *not* an error: the returned
    /// [`AnalyzeResult`] carries them in `unresolved_indirect_branches`
    /// (with their `IndirectBranch` placeholder nodes still in the
    /// function), so a caller wanting full resolution asserts that list is
    /// empty.
    ///
    /// `cc` is the function-default calling convention (already resolved
    /// against this handle's register table).  `lift_opts` supplies the
    /// caller's CFG/lift configuration (`cfg.fn_max_size`,
    /// `cfg.allow_code_before_start_addr`, `per_address_ccs`, and
    /// `compact` — applied after the pipeline at finalize); its
    /// `cfg.known_targets` seed is ignored — the loop grows its own.
    /// `opt_opts` supplies the optimiser configuration (`alias_mode`,
    /// `calls_clobber`).
    ///
    /// `pipeline` lets the caller control which optimisations run: pass
    /// `Some(p)` to use a custom [`strider_opt::OptimizerPipeline`], or
    /// `None` for [`strider_opt::default_pipeline`].  Either way the
    /// orchestrator appends its own [`strider_opt::IndirectBranchClassify`]
    /// post-pass (the resolution mechanism is not user-tunable), and the
    /// pipeline is built once and reused across every re-lift.
    ///
    /// # Errors
    ///
    /// Returns an error only for genuine lift / cfg / opt / validation
    /// failures — never for an unresolvable indirect branch.
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        lift_opts: &LiftOptions,
        opt_opts: &OptOptions,
        pipeline: Option<strider_opt::OptimizerPipeline>,
    ) -> Result<AnalyzeResult> {
        let start_addr = MachineInsnAddr::from(entry);
        // Build the optimiser pipeline once and reuse it across re-lifts
        // (`OptimizerPipeline::run` takes `&self`).  Default when the caller
        // passed none; the indirect-branch classifier post-pass is always
        // appended — it is the orchestrator's resolution mechanism, not a
        // user-tunable optimisation.
        let mut pipeline = pipeline.unwrap_or_else(strider_opt::default_pipeline);
        pipeline.add_post_pass(strider_opt::IndirectBranchClassify);
        // The single working `LiftOptions` carried across iterations.
        // `known_targets` starts empty and GROWS in place as branches
        // resolve; `fn_max_size` / `allow_code_before_start_addr` /
        // `per_address_ccs` are copied from the caller once. The
        // tracked-varnode set is scanned fresh from each rebuilt CFG inside
        // the lifter, and `cc` is threaded per lift call (the reused
        // `Lifter` engine does not store it).
        let mut working = LiftOptions {
            cfg: strider_cfg::CfgOptions {
                fn_max_size: lift_opts.cfg.fn_max_size,
                allow_code_before_start_addr: lift_opts.cfg.allow_code_before_start_addr,
                known_targets: FxHashMap::default(),
            },
            per_address_ccs: lift_opts.per_address_ccs.clone(),
            // `compact` is a finalize-only knob (the lift methods ignore
            // it); the post-loop finalize step below reads
            // `lift_opts.compact` directly.  Pinned `false` here so the
            // in-loop `working` clone carries only loop-relevant knobs and
            // no future edit can accidentally read a stale duplicate.
            compact: false,
        };

        let (mut cfg, mut function, mut unresolved, mut resolutions) =
            self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
        // Each non-terminal iteration folds the classifier's results into
        // `known_targets` and re-lifts.  The loop terminates as soon as
        // nothing new resolves (`apply_resolutions` returns `false`).  The
        // cap is only a backstop; `converged` records whether the loop hit
        // a natural fixed point (vs. exhausting the cap) so we can surface
        // non-convergence rather than silently returning a stale result.
        let mut converged = false;
        for _ in 0..MAX_RESOLUTION_ITERATIONS {
            if unresolved.is_empty() {
                converged = true;
                break;
            }
            // `known_targets` is the SSoT for "already-classified" sites:
            // `apply_resolutions` only folds in *new* classifications and
            // never re-defers a site whose entry already matches, so a
            // `Multiple`-with-OOB table (which the cfg builder re-emits as an
            // unresolved placeholder) cannot churn the loop — its second
            // identical classification is a no-op and the loop converges.
            if !apply_resolutions(&mut working.cfg.known_targets, &unresolved, resolutions)? {
                converged = true;
                break;
            }
            (cfg, function, unresolved, resolutions) =
                self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
        }

        // The cap is the crate's core-invariant backstop — falling
        // through it means the resolve/re-lift loop never reached a fixed
        // point, which is a bug (a pathological classifier/cfg oscillation),
        // not an unresolvable branch.  Surface it loudly in debug builds
        // rather than returning a silently-truncated result.
        debug_assert!(
            converged,
            "indirect-branch resolution did not converge within \
             MAX_RESOLUTION_ITERATIONS={MAX_RESOLUTION_ITERATIONS}; \
             returning a possibly-stale result"
        );
        let _ = converged;

        // Report a site as unresolved only when its
        // `IndirectBranch` placeholder is still LIVE in the final function
        // AND the loop never folded a classification for it into
        // `known_targets`.  A placeholder the node-removing passes proved
        // dead is silently dropped (matching the classifier's contract); a
        // site that *was* classified but the cfg layer could not seat (an
        // OOB `Multiple` table) is treated as resolved-but-unseatable, not
        // unresolved.  This MUST run before `compact`, since `unresolved`'s
        // `NodeId`s index into the un-renumbered function.
        let unresolved_indirect_branches =
            live_unresolved_branches(&function, &unresolved, &working.cfg.known_targets);

        if lift_opts.compact {
            function.compact()?;
        }
        Ok(AnalyzeResult {
            cfg,
            function,
            unresolved_indirect_branches,
        })
    }

    /// Build the CFG, lift to IR, and run `pipeline` (which already carries
    /// the analysis-only [`strider_opt::IndirectBranchClassify`] post-pass,
    /// appended once by [`Self::analyze`]).
    /// Returns `(cfg, function, unresolved, resolutions)`: the CFG the
    /// function was lifted from, the optimised IR, the lift-time
    /// deferred-anchor list (each `BranchIndirect`'s pcode address paired
    /// with its `IndirectBranch` placeholder `NodeId`), and the post-pass's
    /// node-keyed classification map. The Sleigh stays owned by
    /// `self.lifter` across calls; `pipeline` is reused across re-lifts
    /// (`run` takes `&self`).
    ///
    /// # Errors
    ///
    /// Propagates CFG-build, lift (including IR validation), and pipeline
    /// failures.
    fn build_lift(
        &mut self,
        start_addr: MachineInsnAddr,
        cc: &strider_target::BuiltCallingConvention,
        working: &LiftOptions,
        opt_opts: &OptOptions,
        pipeline: &strider_opt::OptimizerPipeline,
    ) -> Result<(
        strider_cfg::Cfg,
        strider_ir::Function,
        UnresolvedAnchors,
        IndirectResolutions,
    )> {
        // Split the `Strider` borrow: the lifter takes `&mut` (build + lift
        // the CFG) while the optimiser ctx takes `&rom` (disjoint fields).
        let Strider {
            ref mut lifter,
            ref rom,
        } = *self;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        // No cfg-time resolver: every `BranchIndirect` not yet in
        // `known_targets` is deferred via `UnresolvedIndirectBranch` and
        // resolved at the full-function IR level by the classifier post-pass.
        let cfg = lifter.build_cfg(start_addr, &working.cfg, &working.per_address_ccs)?;
        // `build_ir_with` takes `cc` by value (it is moved all the way into
        // `Function::default_cc` — see `strider-ir`'s `FunctionBuilder::new`).
        // `analyze`'s resolve/re-lift loop calls `build_lift` — and thus this
        // — more than once with the same caller-owned `cc`, so this clone is
        // the one unavoidable boundary clone the by-value threading pushes
        // out to: `Strider::analyze`'s iteration shape needs `cc` again on
        // the next re-lift.
        let LiftOutcome {
            mut function,
            unresolved_branches: unresolved,
            ..
        } = lifter.build_ir_with(&cfg, cc.clone(), working)?;

        let mut ctx = OptCtx::new(rom_ref);
        ctx.options = opt_opts.clone();
        pipeline.run(&mut function, &mut ctx)?;
        let resolutions = std::mem::take(&mut ctx.indirect_resolutions);

        Ok((cfg, function, unresolved, resolutions))
    }
}

/// The result of [`Strider::analyze`]: the final CFG plus the optimised IR
/// plus the indirect-branch sites that could not be resolved.
///
/// `unresolved_indirect_branches` is empty when every indirect branch was
/// resolved to concrete targets; otherwise it lists the pcode address of
/// each branch whose `IndirectBranch` placeholder is still present in
/// `function`.  A caller wanting full resolution asserts it is empty.
pub struct AnalyzeResult {
    /// The CFG from the FINAL resolve/re-lift iteration — the one
    /// `function` was actually lifted from (resolved indirect branches are
    /// seated as real terminators via `known_targets`, so this is the SSoT
    /// CFG matching the returned IR).
    pub cfg: strider_cfg::Cfg,
    /// The optimised IR.  May still contain `IndirectBranch` placeholder
    /// nodes for any site in `unresolved_indirect_branches`.
    pub function: strider_ir::Function,
    /// The pcode addresses of indirect branches that could not be resolved
    /// (sorted, deduplicated).
    pub unresolved_indirect_branches: Vec<PcodeInsnAddr>,
}

/// Backstop bound on resolve-and-re-lift iterations.  The loop normally
/// stops far sooner — as soon as an iteration resolves nothing new (see
/// [`apply_resolutions`]) — since every continued iteration strictly grows
/// the bounded `known_targets` map.  This cap only guards against a
/// pathological classifier that keeps reporting "growth" without
/// converging.
const MAX_RESOLUTION_ITERATIONS: usize = 256;

/// Fold the classifier post-pass's `resolutions` into `known_targets`,
/// keyed back to pcode addresses via `unresolved`.  Returns whether the
/// induced edge set grew (i.e. at least one new branch resolved) — the
/// loop's progress signal.
///
/// Convergence: a site already present in `known_targets` is
/// **terminal** — its classification was folded in on an earlier
/// iteration.  If its placeholder still reappears in `unresolved` (the cfg
/// builder could not seat the terminator, e.g. an out-of-range `Multiple`
/// table that it re-emits as an unresolved placeholder), we do **not**
/// re-fold a fresh classification for it.  This stops a classifier whose
/// re-lifted graph yields a *different* target set each iteration (value-
/// range widening) from churning the edge set to the iteration cap: each
/// stuck site contributes its classification exactly once.
///
/// Collision: two distinct `IndirectBranch` placeholders sharing one
/// `PcodeInsnAddr` (overlapping/duplicated code terminating two regions at
/// the same machine address) both fold onto the same `known_targets` entry.
/// Rather than let the last write silently win, their target sets are
/// **merged** (the union of `Multiple` targets; a `LinkRegister` /
/// `Single` clash widens to `Multiple`) so the seated terminator covers
/// every classified successor.
///
/// # Errors
///
/// Returns an error if the post-pass classified an `IndirectBranch` node
/// that has no recorded lift-time pcode address (an internal-consistency
/// violation that should never occur).
fn apply_resolutions(
    known_targets: &mut FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    unresolved: &UnresolvedAnchors,
    resolutions: IndirectResolutions,
) -> Result<bool> {
    let node_to_addr: FxHashMap<strider_ir::node::NodeId, PcodeInsnAddr> = unresolved
        .iter()
        .map(|(addr, node)| (*node, *addr))
        .collect();
    let prev_edge_set = edge_set_of(known_targets);

    // Stage this iteration's new classifications per-address first, merging
    // same-address collisions before touching `known_targets`.
    let mut staged: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    for (node, resolved) in resolutions {
        let Some(targets) = resolved else { continue };
        let addr = node_to_addr.get(&node).copied().ok_or_else(|| {
            anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
        })?;
        match staged.entry(addr) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(targets);
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let merged = merge_resolved(e.get(), &targets);
                e.insert(merged);
            }
        }
    }
    // Convergence without dropping improvements: re-deriving the SAME
    // classification for an already-present site re-inserts an equal value
    // (a no-op for the edge set, so an unchanged cone converges instead of
    // churning to the iteration cap), while a genuinely DIFFERENT
    // classification — e.g. a previously-unseatable target set that narrows
    // to a seatable one once other branches resolve — overwrites the stale
    // entry.  The progress signal is the set-diff below, not the insert, so
    // an unconditional insert is correct: only `edge_set_of` and the cfg
    // builder read `known_targets`, and both are insensitive to re-inserting
    // an equal value.
    known_targets.extend(staged);
    Ok(edge_set_of(known_targets) != prev_edge_set)
}

/// Expands a `ResolvedTargets` into its successor set as an iterator of
/// `Option<u64>`: `LinkRegister → [None]` (no concrete successor address),
/// `Single(k) → [Some(k)]`, `Multiple(ks) → Some(k)` for each `k`.  The
/// single source of truth for the three-arm match that both
/// [`merge_resolved`] and [`edge_set_of`] perform.
fn targets_of(r: &ResolvedTargets) -> impl Iterator<Item = Option<u64>> + '_ {
    // `head` yields 0-or-1 items (None/Single), `tail` yields the Multiple
    // slice; their chain unifies the three arms into one return type with
    // no allocation.
    let (head, tail): (Option<Option<u64>>, &[u64]) = match r {
        ResolvedTargets::LinkRegister => (Some(None), &[]),
        ResolvedTargets::Single(k) => (Some(Some(*k)), &[]),
        ResolvedTargets::Multiple(ks) => (None, ks.as_slice()),
    };
    head.into_iter().chain(tail.iter().map(|k| Some(*k)))
}

/// Merge two `ResolvedTargets` classifications for the same pcode address
/// (same-address collision) into one whose target set is the union of
/// both.  Two `LinkRegister`s stay `LinkRegister`; otherwise every concrete
/// successor address is unioned and the result widens to `Single` (one
/// distinct target) or `Multiple` (more than one).  Order-independent.
fn merge_resolved(a: &ResolvedTargets, b: &ResolvedTargets) -> ResolvedTargets {
    if matches!(a, ResolvedTargets::LinkRegister) && matches!(b, ResolvedTargets::LinkRegister) {
        return ResolvedTargets::LinkRegister;
    }
    // `flatten` drops the `None`s (LinkRegister contributes no concrete
    // successor), keeping only the unioned target addresses.
    let mut targets: Vec<u64> = targets_of(a).chain(targets_of(b)).flatten().collect();
    targets.sort_unstable();
    targets.dedup();
    match targets.as_slice() {
        [single] => ResolvedTargets::Single(*single),
        _ => ResolvedTargets::Multiple(targets),
    }
}

/// The pcode addresses to report as unresolved indirect branches: the
/// lift-time deferred sites filtered down to those that are genuinely
/// *live-and-unclassified* in the final function (sorted, deduplicated).
///
/// A lift-time placeholder is **excluded** from the report when either:
///   * it is no longer live in `function` — the node-removing optimizer
///     passes proved it unreachable, so it needs no resolution (matching
///     [`strider_opt::IndirectBranchClassify`]'s "silently dropped"
///     contract); or
///   * the loop already folded a classification for its address into
///     `known_targets` — the site *was* classified, even if the cfg layer
///     could not seat the terminator (e.g. an out-of-range `Multiple`
///     table, which it re-emits as an unresolved placeholder).  Reporting
///     such a site as unresolved would contradict the fact that the
///     orchestrator did resolve it.
fn live_unresolved_branches(
    function: &strider_ir::Function,
    unresolved: &UnresolvedAnchors,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Vec<PcodeInsnAddr> {
    use strider_ir::node::NodeKind;
    use strider_ir::{IRViewer, IRWalker};

    // The live `IndirectBranch` placeholder nodes still reachable from the
    // entry (one cheap reachability walk shared across every anchor).
    let live_indirect: rustc_hash::FxHashSet<strider_ir::node::NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::IndirectBranch))
        .collect();

    let mut out: Vec<PcodeInsnAddr> = unresolved
        .iter()
        .filter(|(addr, node)| live_indirect.contains(node) && !known_targets.contains_key(addr))
        .map(|(addr, _node)| *addr)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Lift-time correlation: each deferred `BranchIndirect`'s pcode address
/// paired with the `NodeId` of the `IndirectBranch` placeholder lifted for
/// it.
type UnresolvedAnchors = Vec<(PcodeInsnAddr, strider_ir::node::NodeId)>;

/// Classifier post-pass output: each live `IndirectBranch` placeholder
/// mapped to its classification (`None` = unresolvable this iteration).
type IndirectResolutions = FxHashMap<strider_ir::node::NodeId, Option<ResolvedTargets>>;

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
        for target in targets_of(resolved) {
            edges.insert((*addr, target));
        }
    }
    edges
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

    // ── live-unresolved filtering ─────────────────────────────────────────

    use strider_ir::node::NodeId;

    /// Build a minimal valid function whose entry region terminates in a
    /// single `IndirectBranch` placeholder anchoring an `IntConst`.  Returns
    /// the function and the placeholder's live `NodeId`.
    fn fn_with_live_indirect_branch() -> (strider_ir::Function, NodeId) {
        use strider_ir::IRBuilderExt;
        let mut b = strider_ir_test_utils::empty_builder().expect("builder");
        let region = b.create_region().expect("region");
        b.set_entry_region(region).expect("entry");
        b.set_region(region);
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let target = b
            .build_int_const(0u128, strider_ir::ValueType::I64)
            .expect("const");
        let placeholder = b.build_indirect_branch(target).expect("indirect");
        b.set_lift_addr(None);
        let function = b.build().expect("build");
        (function, placeholder)
    }

    #[test]
    fn live_unresolved_reports_live_unclassified_branch() {
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        // Live placeholder, never classified → reported.
        assert_eq!(
            live_unresolved_branches(&function, &unresolved, &known),
            vec![addr]
        );
    }

    #[test]
    fn live_unresolved_excludes_dead_branch() {
        // A lift-time anchor whose `NodeId` is NOT a live
        // `IndirectBranch` in the final graph (e.g. the optimizer culled it,
        // here simulated by pairing the address with a non-IndirectBranch
        // live node — the function's entry node) must NOT be reported.
        let (function, _node) = fn_with_live_indirect_branch();
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        let non_indirect = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Entry))
            .expect("entry node");
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, non_indirect)];
        let known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        assert!(
            live_unresolved_branches(&function, &unresolved, &known).is_empty(),
            "a dead / non-live IndirectBranch placeholder must not be reported"
        );
    }

    #[test]
    fn live_unresolved_excludes_already_classified_branch() {
        // A site whose address is already in `known_targets` (it was
        // classified, even if the cfg layer re-emitted its placeholder) is
        // treated as resolved-but-unseatable, not unresolved.
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(addr, ResolvedTargets::Multiple(vec![0x2000, 0x9999_0000]));
        assert!(
            live_unresolved_branches(&function, &unresolved, &known).is_empty(),
            "a classified (in known_targets) site must not be reported unresolved"
        );
    }

    // ── apply_resolutions convergence ─────────────────────────────────────

    #[test]
    fn apply_resolutions_skips_identical_reclassification_but_applies_improved() {
        // Convergence WITHOUT dropping improvements: re-deriving the SAME
        // classification for an already-present site is a no-op (so a site whose
        // cone is unchanged converges instead of churning), but a genuinely
        // DIFFERENT classification — e.g. a previously-unseatable set that
        // narrows to a seatable one once other branches resolve — IS applied.
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];

        // (a) identical re-classification → no change, loop can converge.
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(addr, ResolvedTargets::Multiple(vec![0x2000, 0x3000]));
        let mut same: IndirectResolutions = FxHashMap::default();
        same.insert(node, Some(ResolvedTargets::Multiple(vec![0x2000, 0x3000])));
        let grew = apply_resolutions(&mut known, &unresolved, same).expect("apply");
        assert!(!grew, "identical re-classification must not report growth");
        assert_eq!(
            known[&addr],
            ResolvedTargets::Multiple(vec![0x2000, 0x3000])
        );

        // (b) an improved (different) classification IS applied, so an
        // unseatable-then-seatable site is not stranded unresolved.
        let mut improved: IndirectResolutions = FxHashMap::default();
        improved.insert(node, Some(ResolvedTargets::Multiple(vec![0x2000, 0x2004])));
        let grew = apply_resolutions(&mut known, &unresolved, improved).expect("apply");
        assert!(grew, "an improved classification must be applied");
        assert_eq!(
            known[&addr],
            ResolvedTargets::Multiple(vec![0x2000, 0x2004])
        );
    }

    #[test]
    fn apply_resolutions_first_classification_grows() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(ResolvedTargets::Single(0x2000)));
        let grew = apply_resolutions(&mut known, &unresolved, resolutions).expect("apply");
        assert!(grew, "a first-time classification must register as growth");
        assert_eq!(known[&addr], ResolvedTargets::Single(0x2000));
    }

    // ── same-address collision merge ──────────────────────────────────────

    #[test]
    fn merge_resolved_unions_multiple_targets() {
        let merged = merge_resolved(
            &ResolvedTargets::Multiple(vec![0x1000, 0x2000]),
            &ResolvedTargets::Multiple(vec![0x2000, 0x3000]),
        );
        assert_eq!(
            merged,
            ResolvedTargets::Multiple(vec![0x1000, 0x2000, 0x3000])
        );
    }

    #[test]
    fn merge_resolved_two_link_registers_stay_link_register() {
        assert_eq!(
            merge_resolved(
                &ResolvedTargets::LinkRegister,
                &ResolvedTargets::LinkRegister
            ),
            ResolvedTargets::LinkRegister
        );
    }

    #[test]
    fn merge_resolved_single_plus_single_widens_to_multiple() {
        assert_eq!(
            merge_resolved(
                &ResolvedTargets::Single(0x1000),
                &ResolvedTargets::Single(0x2000)
            ),
            ResolvedTargets::Multiple(vec![0x1000, 0x2000])
        );
    }

    #[test]
    fn apply_resolutions_merges_two_anchors_at_same_addr() {
        // Two DISTINCT placeholder `NodeId`s mapped to one shared
        // `PcodeInsnAddr` classify independently; `apply_resolutions` merges
        // (unions) their target sets into the single `known_targets` entry
        // rather than letting the second insert silently overwrite the first.
        // We use the two distinct live nodes a single function already owns
        // (the `IntConst` value node and the `IndirectBranch` placeholder) as
        // the two anchor keys — `apply_resolutions` keys purely off `NodeId`,
        // never node kind.
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        let (function, indirect) = fn_with_live_indirect_branch();
        let other = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .expect("const node");
        assert_ne!(indirect, other, "need two distinct NodeIds");
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, indirect), (addr, other)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(indirect, Some(ResolvedTargets::Single(0x2000)));
        resolutions.insert(other, Some(ResolvedTargets::Single(0x3000)));
        apply_resolutions(&mut known, &unresolved, resolutions).expect("apply");
        assert_eq!(
            known[&addr],
            ResolvedTargets::Multiple(vec![0x2000, 0x3000]),
            "two same-addr classifications must be merged, not overwritten"
        );
    }
}
