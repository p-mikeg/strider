#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

pub use strider_opt as opt;

pub use strider_lift::LiftOptions;
pub use strider_lift::lift::{LiftOutcome, Lifter};

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_opt::{OptCtx, OptOptions, ReadOnlyMemory};

/// Per-binary analysis handle.
pub struct Strider<R>
where
    R: rsleigh::MemReader,
{
    lifter: Lifter<R>,
    /// `LoadReadOnly`'s memory image; `None` disables the pass.
    rom: Option<Box<dyn ReadOnlyMemory>>,
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
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

    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lifter.sleigh_regs()
    }

    /// The same `Sleigh` instance `analyze` / `build_cfg` drive.
    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        self.lifter.sleigh()
    }

    /// Structural CFG build only: no lift, no optimisation, no
    /// indirect-branch resolution.
    ///
    /// # Errors
    ///
    /// Propagates CFG-build failures (see [`Lifter::build_cfg`]).
    pub fn build_cfg(
        &mut self,
        entry: u64,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        // Per-address CCs are a lift-time concept, threaded only through
        // `analyze` / `build_lift`.
        self.lifter.build_cfg(
            MachineInsnAddr::from(entry),
            cfg_opts,
            &rustc_hash::FxHashMap::default(),
        )
    }

    /// Lift, optimise, and resolve indirect branches at `entry`.
    ///
    /// Resolution is a fixed-point loop: each iteration classifies indirect
    /// branches against the optimised IR and folds resolved targets into
    /// `known_targets` (keys are only added, but a re-classified site's target
    /// set may be overwritten, even narrowed), then re-lifts. It converges when
    /// the induced edge set stops changing. `MAX_RESOLUTION_ITERATIONS` is a
    /// backstop.
    ///
    /// Unresolvable branches are a RESULT, not an error: they come back in
    /// [`AnalyzeResult::unresolved_indirect_branches`] with their
    /// placeholder nodes still in the function.
    ///
    /// `lift_opts.cfg.known_targets` is IGNORED: the loop grows its own
    /// map. `lift_opts.compact` applies once at finalize, after the loop.
    ///
    /// `pipeline` picks the optimisations ([`strider_opt::default_pipeline`]
    /// when `None`); the [`strider_opt::IndirectBranchClassify`] post-pass is
    /// always appended.
    ///
    /// # Errors
    ///
    /// Only genuine lift / cfg / opt / validation failures. Never an
    /// unresolvable indirect branch.
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        lift_opts: &LiftOptions,
        opt_opts: &OptOptions,
        pipeline: Option<strider_opt::OptimizerPipeline>,
    ) -> Result<AnalyzeResult> {
        let start_addr = MachineInsnAddr::from(entry);
        // Built once and reused across re-lifts (`run` takes `&self`).
        let mut pipeline = pipeline.unwrap_or_else(strider_opt::default_pipeline);
        pipeline.add_post_pass(strider_opt::IndirectBranchClassify);
        // Carried across iterations; only `known_targets` mutates.
        let mut working = LiftOptions {
            cfg: strider_cfg::CfgOptions {
                fn_max_size: lift_opts.cfg.fn_max_size,
                allow_code_before_start_addr: lift_opts.cfg.allow_code_before_start_addr,
                known_targets: FxHashMap::default(),
            },
            per_address_ccs: lift_opts.per_address_ccs.clone(),
            // Finalize-only knob; the post-loop step reads
            // `lift_opts.compact` directly. Pinned false so no future edit
            // can read a stale duplicate off `working`.
            compact: false,
        };

        let (mut cfg, mut function, mut unresolved, mut resolutions) =
            self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
        // Must be snapshotted in lockstep with `function`, and BEFORE
        // `resolutions` is moved into `apply_resolutions`. The classifier
        // already walked for these, so reusing its keys saves
        // `live_unresolved_branches` a per-iteration reachability walk.
        let mut live_indirect: rustc_hash::FxHashSet<strider_ir::node::NodeId> =
            resolutions.keys().copied().collect();
        let mut converged = false;
        for _ in 0..MAX_RESOLUTION_ITERATIONS {
            if unresolved.is_empty() {
                converged = true;
                break;
            }
            if !apply_resolutions(&mut working.cfg.known_targets, &unresolved, resolutions)? {
                converged = true;
                break;
            }
            (cfg, function, unresolved, resolutions) =
                self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
            live_indirect = resolutions.keys().copied().collect();
        }

        // Falling through the cap means the loop never reached a fixed
        // point: a pathological classifier/cfg oscillation, not an
        // unresolvable branch. Fail loudly rather than truncate silently.
        debug_assert!(
            converged,
            "indirect-branch resolution did not converge within \
             MAX_RESOLUTION_ITERATIONS={MAX_RESOLUTION_ITERATIONS}; \
             returning a possibly-stale result"
        );
        let _ = converged;

        // MUST run before `compact`: `unresolved`'s `NodeId`s index into the
        // un-renumbered function.
        let unresolved_indirect_branches =
            live_unresolved_branches(&live_indirect, &unresolved, &working.cfg.known_targets);

        if lift_opts.compact {
            function.compact()?;
        }
        Ok(AnalyzeResult {
            cfg,
            function,
            unresolved_indirect_branches,
        })
    }

    /// One resolve/re-lift iteration. Returns `(cfg, function, unresolved,
    /// resolutions)`: the CFG `function` was lifted from, the optimised IR,
    /// the lift-time deferred anchors (pcode address paired with the
    /// `IndirectBranch` placeholder's `NodeId`), and the classifier
    /// post-pass's node-keyed classification map.
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
        // Destructured to split the borrow: `lifter` goes out `&mut` while
        // the optimiser ctx holds `&rom`.
        let Strider {
            ref mut lifter,
            ref rom,
        } = *self;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        // No cfg-time resolver: a `BranchIndirect` not yet in
        // `known_targets` is deferred and resolved at the full-function IR
        // level by the classifier post-pass.
        let cfg = lifter.build_cfg(start_addr, &working.cfg, &working.per_address_ccs)?;
        // `cc` is moved all the way into `Function::default_cc`, but the
        // resolve loop calls this again on the next re-lift, so the clone is
        // unavoidable here.
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

pub struct AnalyzeResult {
    /// The FINAL iteration's CFG, i.e. the one `function` was lifted from.
    pub cfg: strider_cfg::Cfg,
    /// May still contain `IndirectBranch` placeholders for any site in
    /// `unresolved_indirect_branches`.
    pub function: strider_ir::Function,
    /// Sorted and deduplicated. Empty means fully resolved.
    pub unresolved_indirect_branches: Vec<PcodeInsnAddr>,
}

/// Backstop only; [`apply_resolutions`] reporting no growth is the real
/// terminator.
const MAX_RESOLUTION_ITERATIONS: usize = 256;

/// Fold `resolutions` into `known_targets`, keyed back to pcode addresses
/// via `unresolved`. Returns whether the induced edge set grew: the loop's
/// progress signal.
///
/// Two distinct placeholders can share one `PcodeInsnAddr`; their target
/// sets are UNIONED, not last-write-wins.
///
/// # Errors
///
/// Errors if a classified `IndirectBranch` has no recorded lift-time pcode
/// address (internal-consistency violation).
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

    // Staged per-address first so same-address collisions merge before
    // anything touches `known_targets`.
    let mut staged: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    for (node, resolved) in resolutions {
        let Some(targets) = resolved else { continue };
        let addr = node_to_addr.get(&node).copied().ok_or_else(|| {
            anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
        })?;
        staged
            .entry(addr)
            .and_modify(|e| *e = merge_resolved(e, &targets))
            .or_insert(targets);
    }
    // Unconditional overwrite, deliberately. Skipping already-present sites
    // would strand a target set that narrows from unseatable to seatable
    // once other branches resolve. Convergence still holds because the
    // progress signal is the edge-set diff below, not the insert: an
    // unchanged cone re-inserts an equal value and reads as no growth.
    known_targets.extend(staged);
    Ok(edge_set_of(known_targets) != prev_edge_set)
}

/// Successor set of a `ResolvedTargets`, where `None` means "no concrete
/// address" (`LinkRegister`).
fn targets_of(r: &ResolvedTargets) -> impl Iterator<Item = Option<u64>> + '_ {
    // Chaining a 0-or-1 `head` with a slice `tail` unifies the three arms
    // into one return type without allocating.
    let (head, tail): (Option<Option<u64>>, &[u64]) = match r {
        ResolvedTargets::LinkRegister => (Some(None), &[]),
        ResolvedTargets::Single(k) => (Some(Some(*k)), &[]),
        ResolvedTargets::Multiple(ks) => (None, ks.as_slice()),
    };
    head.into_iter().chain(tail.iter().map(|k| Some(*k)))
}

/// Union two classifications for the same pcode address. Two
/// `LinkRegister`s stay `LinkRegister`; anything else widens to `Single` or
/// `Multiple` over the unioned addresses. Order-independent.
fn merge_resolved(a: &ResolvedTargets, b: &ResolvedTargets) -> ResolvedTargets {
    if matches!(a, ResolvedTargets::LinkRegister) && matches!(b, ResolvedTargets::LinkRegister) {
        return ResolvedTargets::LinkRegister;
    }
    // `flatten` drops the `None`s: LinkRegister contributes no address.
    let mut targets: Vec<u64> = targets_of(a).chain(targets_of(b)).flatten().collect();
    targets.sort_unstable();
    targets.dedup();
    match targets.as_slice() {
        [single] => ResolvedTargets::Single(*single),
        _ => ResolvedTargets::Multiple(targets),
    }
}

/// Lift-time deferred sites narrowed to the genuinely live-and-unclassified
/// ones (sorted, deduplicated). A site is excluded when its placeholder is no
/// longer live, or when its address is already in `known_targets` (classified,
/// even if the cfg layer could not seat the terminator).
fn live_unresolved_branches(
    live_indirect: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    unresolved: &UnresolvedAnchors,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Vec<PcodeInsnAddr> {
    // `live_indirect` comes from the classifier's own
    // `walk_kind(IndirectBranch)`, so it is exactly the reachable-placeholder
    // set, snapshotted from the `build_lift` that produced `function`.
    let mut out: Vec<PcodeInsnAddr> = unresolved
        .iter()
        .filter(|(addr, node)| live_indirect.contains(node) && !known_targets.contains_key(addr))
        .map(|(addr, _node)| *addr)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Each deferred `BranchIndirect`'s pcode address paired with the `NodeId`
/// of the `IndirectBranch` placeholder lifted for it.
type UnresolvedAnchors = Vec<(PcodeInsnAddr, strider_ir::node::NodeId)>;

/// `None` = unresolvable this iteration.
type IndirectResolutions = FxHashMap<strider_ir::node::NodeId, Option<ResolvedTargets>>;

/// Induced edge set of `known_targets`. `target = None` is a `LinkRegister`
/// resolution.
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

    use strider_ir::node::NodeId;

    /// Minimal valid function: entry region terminating in one
    /// `IndirectBranch` over an `IntConst`. Returns the placeholder's
    /// `NodeId` alongside it.
    fn fn_with_live_indirect_branch() -> (strider_ir::Function, NodeId) {
        use strider_ir::IRBuilderExt;
        let mut b = strider_ir_test_utils::empty_builder().expect("builder");
        let region = b.create_region_all().expect("region");
        b.set_entry_region_all(region).expect("entry");
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

    /// The reachable `IndirectBranch` set; production snapshots the
    /// equivalent from `resolutions.keys()`.
    fn live_indirect_set(function: &strider_ir::Function) -> rustc_hash::FxHashSet<NodeId> {
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        function
            .walk()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::IndirectBranch))
            .collect()
    }

    #[test]
    fn live_unresolved_reports_live_unclassified_branch() {
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let live = live_indirect_set(&function);
        assert_eq!(
            live_unresolved_branches(&live, &unresolved, &known),
            vec![addr]
        );
    }

    #[test]
    fn live_unresolved_excludes_dead_branch() {
        // A culled placeholder, simulated by pairing the address with a live
        // node that is not an `IndirectBranch`.
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
        let live = live_indirect_set(&function);
        assert!(
            live_unresolved_branches(&live, &unresolved, &known).is_empty(),
            "a dead / non-live IndirectBranch placeholder must not be reported"
        );
    }

    #[test]
    fn live_unresolved_excludes_already_classified_branch() {
        // Already in `known_targets` means classified, even though the cfg
        // layer re-emitted the placeholder: resolved-but-unseatable, not
        // unresolved.
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(addr, ResolvedTargets::Multiple(vec![0x2000, 0x9999_0000]));
        let live = live_indirect_set(&function);
        assert!(
            live_unresolved_branches(&live, &unresolved, &known).is_empty(),
            "a classified (in known_targets) site must not be reported unresolved"
        );
    }

    #[test]
    fn apply_resolutions_skips_identical_reclassification_but_applies_improved() {
        // Pins convergence without dropping improvements. An identical
        // re-classification must report no growth (or an unchanged cone
        // churns to the iteration cap), while a narrowed set must still be
        // applied (or an unseatable site is stranded unresolved forever).
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];

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
        // Two distinct placeholders sharing one address must union, not let
        // the second insert overwrite the first. Any two `NodeId`s work as
        // anchor keys since `apply_resolutions` never looks at node kind, so
        // the fixture reuses the function's `IntConst` as the second.
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
