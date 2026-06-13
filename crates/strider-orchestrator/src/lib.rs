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
//! an [`AnalyzeResult`] whose `unresolved_indirect_branches` lists them
//! (their placeholders remain in `function`). `MAX_RESOLUTION_ITERATIONS`
//! is only a backstop — step 3's "nothing new resolved" check is the real
//! terminator, since every continued iteration strictly grows the bounded
//! `known_targets` map.
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

/// Builds the shared [`OptCtx`] for one pipeline run from the
/// orchestrator's borrowed rom slot and the per-run [`OptOptions`].
/// Threaded into every `pipeline.run` site so every iteration of the
/// fixed-point loop sees the same rom image (as the cfg builder) and the
/// same opt configuration (alias precision for every SP-aware pass, plus
/// `calls_clobber_stack_arguments`).
///
/// The byte order used to decode rom bytes is NOT carried here —
/// `LoadReadOnly` reads it from the function's own `Function::endianness`
/// (the SSoT) at decode time.  `sp_memo` starts empty — the pipeline clears
/// it at every drain.
fn opt_ctx_for_run<'mem>(
    rom: Option<&'mem dyn ReadOnlyMemory>,
    opt_opts: &OptOptions,
) -> OptCtx<'mem> {
    let mut ctx = OptCtx::new(rom);
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
    /// `calls_clobber_stack_arguments`).
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
            // Carried for completeness; not used during lifting — the
            // finalize step below reads `lift_opts.compact` directly.
            compact: lift_opts.compact,
        };

        let (mut function, mut unresolved, mut resolutions) =
            self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
        // Each non-terminal iteration folds the classifier's results into
        // `known_targets` and re-lifts. The loop terminates as soon as
        // nothing new resolves (`apply_resolutions` returns `false`); the
        // cap is only a backstop, since every continued iteration strictly
        // grows the bounded `known_targets` map.
        for _ in 0..MAX_RESOLUTION_ITERATIONS {
            if unresolved.is_empty() {
                break;
            }
            if !apply_resolutions(&mut working.cfg.known_targets, &unresolved, resolutions)? {
                break;
            }
            (function, unresolved, resolutions) =
                self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
        }

        if lift_opts.compact {
            function.compact()?;
        }
        let mut unresolved_indirect_branches: Vec<PcodeInsnAddr> =
            unresolved.into_iter().map(|(addr, _node)| addr).collect();
        unresolved_indirect_branches.sort_unstable();
        unresolved_indirect_branches.dedup();
        Ok(AnalyzeResult {
            function,
            unresolved_indirect_branches,
        })
    }

    /// Build the CFG, lift to IR, and run `pipeline` (which already carries
    /// the analysis-only [`strider_opt::IndirectBranchClassify`] post-pass,
    /// appended once by [`Self::analyze`]).
    /// Returns `(function, unresolved, resolutions)`: the optimised IR, the
    /// lift-time deferred-anchor list (each `BranchIndirect`'s pcode address
    /// paired with its `IndirectBranch` placeholder `NodeId`), and the
    /// post-pass's node-keyed classification map. The Sleigh stays owned by
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
    ) -> Result<(strider_ir::Function, UnresolvedAnchors, IndirectResolutions)> {
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
        let cfg = lifter.build_cfg(start_addr, &working.cfg)?;
        let LiftOutcome {
            mut function,
            unresolved_branches: unresolved,
            ..
        } = lifter.build_ir_with(&cfg, cc, working)?;

        let mut ctx = opt_ctx_for_run(rom_ref, opt_opts);
        pipeline.run(&mut function, &mut ctx)?;
        let resolutions = std::mem::take(&mut ctx.indirect_resolutions);

        Ok((function, unresolved, resolutions))
    }
}

/// The result of [`Strider::analyze`]: the optimised IR plus the
/// indirect-branch sites that could not be resolved.
///
/// `unresolved_indirect_branches` is empty when every indirect branch was
/// resolved to concrete targets; otherwise it lists the pcode address of
/// each branch whose `IndirectBranch` placeholder is still present in
/// `function`.  A caller wanting full resolution asserts it is empty.
pub struct AnalyzeResult {
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
    for (node, resolved) in resolutions {
        let Some(targets) = resolved else { continue };
        let addr = node_to_addr.get(&node).copied().ok_or_else(|| {
            anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
        })?;
        known_targets.insert(addr, targets);
    }
    Ok(edge_set_of(known_targets) != prev_edge_set)
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
}
