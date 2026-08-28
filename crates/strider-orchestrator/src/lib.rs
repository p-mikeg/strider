pub use strider_opt as opt;

pub use strider_lift::LiftOptions;
pub use strider_lift::lift::{LiftOutcome, Lifter};

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_opt::{OptCtx, OptOptions, PostOptimizer, ReadOnlyMemory};

/// Per-binary analysis handle.
pub struct Strider<R>
where
    R: rsleigh::MemReader,
{
    arch: strider_target::SleighArch,
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
        Ok(Self { arch, lifter, rom })
    }

    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lifter.sleigh_regs()
    }

    #[must_use]
    pub fn arch(&self) -> strider_target::SleighArch {
        self.arch
    }

    /// Every Sleigh user-op name this architecture can emit, indexed by
    /// `user_op_id`.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        self.lifter.user_op_names()
    }

    /// The same `Sleigh` instance `analyze` / `build_cfg` drive.
    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        self.lifter.sleigh()
    }

    /// The read-only image `LoadReadOnly` folds against, so a caller running
    /// its own pipeline builds the same `OptCtx` `analyze` does.
    #[must_use]
    pub fn rom(&self) -> Option<&dyn ReadOnlyMemory> {
        self.rom.as_deref()
    }

    /// The ISA-mode bit a target seated with no mode of its own decodes in, per
    /// indirect site: what `Builder::enqueue_resolved` reads at that branch
    /// address. `entry_bit` covers a site whose context carries no value yet
    /// and every arch with no ISA-mode var; it comes from
    /// [`strider_cfg::Cfg::function_isa_bit`] so the fallback here is the SAME
    /// derivation the builder uses, not a second one that agrees by accident.
    fn flowing_isa_bits(
        &self,
        unresolved: &UnresolvedAnchors,
        switch_anchors: &UnresolvedAnchors,
        entry_bit: bool,
    ) -> FxHashMap<PcodeInsnAddr, bool> {
        let sleigh = self.lifter.sleigh();
        unresolved
            .iter()
            .chain(switch_anchors.iter())
            .map(|(addr, _)| {
                let bit = strider_cfg::flowing_isa_bit_at(
                    &self.arch,
                    sleigh,
                    addr.machine_addr.addr,
                    entry_bit,
                );
                (*addr, bit)
            })
            .collect()
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
    /// set is overwritten, and may narrow), then re-lifts. It converges when
    /// the induced edge set stops changing. A site NARROWS when its address
    /// projection stops being a superset of the previous round's, or when a
    /// target's proved ISA mode flips; a mode-less target taking a proved mode
    /// at an unchanged address set is growth.
    /// `MAX_RESOLUTION_ITERATIONS` caps the loop and, since one iteration
    /// seats one level of discovery, chain depth. A site that narrows twice has
    /// an answer that depends on what the previous round seated, so no member
    /// of the cycle is trustworthy: it is abandoned and reported.
    ///
    /// Unresolvable branches are a RESULT, not an error: they come back in
    /// [`AnalyzeResult::unresolved_indirect_branches`] with their
    /// placeholder nodes still in the function. So does a site whose fold
    /// could not seat every successor a round proved: it is seated on the arms
    /// it could take, and reported rather than passed off as complete. So does
    /// a site whose answer never settles (resolving it is abandoned rather than
    /// failing the function), and one naming a target that will not decode,
    /// which a misclassified table bound produces.
    ///
    /// That is not the only incompleteness channel. A site the CFG CONSUMED
    /// (a `LinkRegister` answer seated as a `Return`, a single out-of-function
    /// target seated as a `TailCall`) leaves no placeholder and no anchor, so
    /// it cannot appear in `unresolved_indirect_branches` at all; it is named
    /// in [`AnalyzeResult::unverified_seeded_sites`] instead. A CFG-level loss
    /// no indirect site owns comes back in
    /// [`AnalyzeResult::isa_mode_conflicts`] or
    /// [`AnalyzeResult::interior_branch_targets`]. A caller asking "may this
    /// answer be incomplete?" has to read all four.
    ///
    /// `lift_opts.cfg.known_targets` seeds the loop by plain address union
    /// every round, and the caller's own map is never mutated. The WORKING set
    /// it feeds is not monotone: a site whose answer narrows twice, and one
    /// naming a target the CFG could not decode, are dropped from it. The CFG
    /// can lose ground for a second reason: seating changes what the
    /// classifier reads, so a wrong seed can stop the selector deriving and
    /// take the site's real arms with it, which is what
    /// `unverified_seeded_sites` reports. A seed asserts the site is complete,
    /// which suppresses the unresolved report of an unclassifiable seated
    /// `Switch` at that address, but only while the settled answer holds
    /// nothing the seed did not name; it never suppresses what the classifier
    /// derives.
    /// `lift_opts.compact` applies once at finalize, after the loop.
    ///
    /// `pipeline` picks the optimisations ([`strider_opt::default_pipeline`]
    /// when `None`); the [`strider_opt::IndirectBranchClassify`] post-pass is
    /// appended unless the pipeline already runs it.
    ///
    /// # Errors
    ///
    /// Genuine lift / cfg / opt / validation failures only. Never an
    /// indirect branch: every site the loop cannot settle is reported.
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
        let pipeline = with_classify(pipeline.unwrap_or_else(strider_opt::default_pipeline));
        // Carried across iterations; only `known_targets` mutates. Seeded from
        // the caller's answers, which the loop then grows: `apply_resolutions`
        // re-unions the seed every round. A seed asserts the site is settled,
        // which suppresses the report of an unclassifiable seated `Switch`
        // until the site outgrows the seed; it does not suppress a loss of what
        // the classifier derives, nor a live placeholder.
        let mut working = LiftOptions {
            // Cloned whole: naming the fields here would silently drop any
            // field added to `CfgOptions` later from the whole re-lift loop.
            cfg: lift_opts.cfg.clone(),
            per_address_ccs: lift_opts.per_address_ccs.clone(),
            // Finalize-only knob; the post-loop step reads
            // `lift_opts.compact` directly. Pinned false so no future edit
            // can read a stale duplicate off `working`.
            compact: false,
        };

        let (mut cfg, mut function, mut unresolved, mut switch_anchors, mut resolutions) =
            self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
        // Must be snapshotted in lockstep with `function`, and BEFORE
        // `resolutions` is moved into `apply_resolutions`. The classifier
        // already walked for these, so reusing its keys saves
        // `live_unresolved_branches` a per-iteration reachability walk.
        let mut live_indirect: rustc_hash::FxHashSet<strider_ir::node::NodeId> =
            resolutions.keys().copied().collect();
        let mut unclassified = unclassified_nodes(&resolutions);
        let mut converged = false;
        // A round that only ever ADDED successors cannot cycle, so exhausting
        // the budget on one is the depth limit, not an oscillation. One
        // narrowing at a site is a refinement: the classifier over-approximates
        // an index bound, then proves the tighter answer once the loop closes.
        // A site narrowing TWICE has an unstable answer, which is the cycle.
        let mut narrow_rounds: FxHashMap<PcodeInsnAddr, u32> = FxHashMap::default();
        // Sticky: a site that lost ground in ANY round cannot be claimed
        // complete by a later one that merely re-derives the smaller set. Also
        // carries the sites the loop abandoned, whose arms are frozen at
        // whatever the round before it gave up had seated.
        let mut derived_incomplete: Vec<PcodeInsnAddr> =
            seated_arm_losses(&cfg, &working.cfg.known_targets);
        let mut still_growing: Vec<PcodeInsnAddr> = Vec::new();
        // Sticky for the same reason `derived_incomplete` is: a round's
        // inexact edge fed the classifier, whose derived targets persist into
        // every later round, so a final CFG without one does not mean none was
        // used.
        let mut interior: Vec<PcodeInsnAddr> = cfg.interior_branch_targets().to_vec();
        // Sticky as `interior` is, and for a sharper reason: `abandon_undecodable`
        // drops the very site whose seat raised the clash, so the NEXT round
        // rebuilds without the edge that decoded those bytes twice. Reading the
        // final cfg alone launders the round that decoded them.
        let mut isa_conflicts: Vec<PcodeInsnAddr> = cfg.isa_mode_conflicts().to_vec();
        // Sites this loop has given up on: their answer never settled, or it
        // named an address that would not decode. Abandoning one leaves its
        // `IndirectBranch` a live placeholder, which is reported as unresolved
        // (a result, the way the contract promises, instead of an error that
        // loses the whole function).
        let mut abandoned: rustc_hash::FxHashSet<PcodeInsnAddr> = rustc_hash::FxHashSet::default();
        derived_incomplete.extend(abandon_undecodable(
            &cfg,
            &mut abandoned,
            &mut working.cfg.known_targets,
        ));
        for _ in 0..MAX_RESOLUTION_ITERATIONS {
            // Seated `Switch` sites are re-derived every round, so the loop
            // runs on while either anchor set is non-empty: a table that
            // resolved before its loop closed widens here.
            if unresolved.is_empty() && switch_anchors.is_empty() {
                converged = true;
                break;
            }
            let flowing = self.flowing_isa_bits(
                &unresolved,
                &switch_anchors,
                cfg.function_isa_bit().unwrap_or(false),
            );
            let progress = apply_resolutions(
                &mut working.cfg.known_targets,
                &lift_opts.cfg.known_targets,
                &unresolved,
                &switch_anchors,
                // Taken, not cloned: every `break` out of this loop sets
                // `converged`, and the post-loop fold reads `resolutions` only
                // when it did not, by which point `build_lift` has reassigned
                // it.
                std::mem::take(&mut resolutions),
                &flowing,
                &abandoned,
            )?;
            for addr in &progress.narrowed {
                let seen = narrow_rounds.entry(*addr).or_default();
                *seen += 1;
                // One narrowing is a refinement. A second means the answer
                // depends on what the previous round seated, so no member of
                // the cycle is trustworthy: stop resolving the site and report
                // it, rather than failing the function.
                if *seen > 1 && abandoned.insert(*addr) {
                    working.cfg.known_targets.remove(addr);
                    derived_incomplete.push(*addr);
                }
            }
            derived_incomplete.extend(progress.derived_incomplete);
            if !progress.changed {
                converged = true;
                break;
            }
            // Only this round's growth: a site that settled earlier is not
            // "still growing when the cap ran out", which is what the report
            // means.
            still_growing.clear();
            still_growing.extend(progress.grew);
            (cfg, function, unresolved, switch_anchors, resolutions) =
                self.build_lift(start_addr, cc, &working, opt_opts, &pipeline)?;
            // BEFORE `abandon_undecodable`, which removes the site from
            // `known_targets`, the map `seated_arm_losses` looks the site up
            // in. Reversed, every site abandoned this round loses its arm-loss
            // report, and for a classifier-derived table that is the only
            // channel that fires.
            derived_incomplete.extend(seated_arm_losses(&cfg, &working.cfg.known_targets));
            derived_incomplete.extend(abandon_undecodable(
                &cfg,
                &mut abandoned,
                &mut working.cfg.known_targets,
            ));
            interior.extend_from_slice(cfg.interior_branch_targets());
            isa_conflicts.extend_from_slice(cfg.isa_mode_conflicts());
            live_indirect = resolutions.keys().copied().collect();
            unclassified = unclassified_nodes(&resolutions);
        }

        // The cap is spent on a `build_lift` whose classifications nothing has
        // folded yet, so the last round's own verdict is only known by folding
        // them. Against a COPY: the returned `cfg` was built from
        // `known_targets` as it stands, and this is a report, not a decision.
        // Kept, where the fold that produced it is discarded: it is the
        // settled answer BOTH seed-aware reports are read against, and reading
        // one against the pre-fold set would call a site that the last round
        // grew past its seed "exactly the seed".
        let mut final_targets = None;
        if !converged {
            let mut folded = working.cfg.known_targets.clone();
            let flowing = self.flowing_isa_bits(
                &unresolved,
                &switch_anchors,
                cfg.function_isa_bit().unwrap_or(false),
            );
            let progress = apply_resolutions(
                &mut folded,
                &lift_opts.cfg.known_targets,
                &unresolved,
                &switch_anchors,
                resolutions,
                &flowing,
                &abandoned,
            )?;
            // `apply_resolutions` already pushes every narrowed address here.
            derived_incomplete.extend(progress.derived_incomplete);
            still_growing.clear();
            still_growing.extend(progress.grew);
            final_targets = Some(folded);
        }

        let budget_exhausted: &[PcodeInsnAddr] = if converged { &[] } else { &still_growing };
        let settled = final_targets.as_ref().unwrap_or(&working.cfg.known_targets);

        let unresolved_indirect_branches = live_unresolved_branches(
            &live_indirect,
            &unresolved,
            &switch_anchors,
            &unclassified,
            &lift_opts.cfg.known_targets,
            settled,
            DerivedChannels {
                budget_exhausted,
                incomplete_derived: &derived_incomplete,
            },
        );
        let unverified_seeded = unverified_seeded_sites(
            &cfg,
            &switch_anchors,
            &unclassified,
            &lift_opts.cfg.known_targets,
            settled,
        );

        if lift_opts.compact {
            function.compact()?;
        }
        let mut isa_mode_conflicts = isa_conflicts;
        isa_mode_conflicts.sort_unstable();
        isa_mode_conflicts.dedup();
        let mut interior_branch_targets = interior;
        interior_branch_targets.sort_unstable();
        interior_branch_targets.dedup();
        Ok(AnalyzeResult {
            cfg,
            function,
            unresolved_indirect_branches,
            isa_mode_conflicts,
            interior_branch_targets,
            unverified_seeded_sites: unverified_seeded,
        })
    }

    /// One resolve/re-lift iteration: the CFG `function` was lifted from, the
    /// optimised IR, the lift-time deferred anchors, the seated-`Switch`
    /// anchors, and the classifier post-pass's node-keyed classification map.
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
        UnresolvedAnchors,
        IndirectResolutions,
    )> {
        // Destructured to split the borrow: `lifter` goes out `&mut` while
        // the optimiser ctx holds `&rom`.
        let Strider {
            ref mut lifter,
            ref rom,
            ..
        } = *self;
        let rom_ref: Option<&dyn ReadOnlyMemory> = rom.as_deref();

        // A `BranchIndirect` outside `known_targets` is deferred here and
        // resolved at the full-function IR level by the classifier post-pass.
        let cfg = lifter.build_cfg(start_addr, &working.cfg, &working.per_address_ccs)?;
        // `cc` is moved all the way into `Function::default_cc`, but the
        // resolve loop calls this again on the next re-lift, so the clone is
        // unavoidable here.
        let LiftOutcome {
            mut function,
            unresolved_branches: unresolved,
            switch_anchors,
            ..
        } = lifter.build_ir_with(&cfg, cc.clone(), working)?;

        let mut ctx = OptCtx::new(rom_ref);
        ctx.options = opt_opts.clone();
        pipeline.run(&mut function, &mut ctx)?;
        let resolutions = std::mem::take(&mut ctx.indirect_resolutions);

        Ok((cfg, function, unresolved, switch_anchors, resolutions))
    }
}

pub struct AnalyzeResult {
    /// The FINAL iteration's CFG, i.e. the one `function` was lifted from.
    pub cfg: strider_cfg::Cfg,
    /// May still contain `IndirectBranch` placeholders for any site in
    /// `unresolved_indirect_branches`.
    pub function: strider_ir::Function,
    /// Sorted and deduplicated. Empty means fully resolved, but not that the
    /// answer is complete: a site the CFG consumed as a `Return` or `TailCall`
    /// can only be reported through `unverified_seeded_sites`.
    pub unresolved_indirect_branches: Vec<PcodeInsnAddr>,
    /// Addresses ANY round's cfg reached carrying two different ISA modes.
    ///
    /// One region owns the bytes, decoded in whichever mode won the work
    /// queue, so the losing edge's path is not the instruction stream it
    /// believes. These are not indirect-branch sites (a direct edge produces
    /// them too), so they are reported here rather than in
    /// `unresolved_indirect_branches`. Non-empty means part of some round's
    /// decode was in a mode a path into it disagreed with; the classifier ran
    /// on that decode whether or not the final cfg still carries the edge.
    pub isa_mode_conflicts: Vec<PcodeInsnAddr>,
    /// Branch targets interior to a region but off every instruction boundary.
    ///
    /// No region can start there (decoding from inside an instruction yields a
    /// different stream), so the edge is seated on the region that owns the
    /// bytes, whose instructions start earlier. Like `isa_mode_conflicts` a
    /// direct edge produces these, so they are reported here rather than in
    /// `unresolved_indirect_branches`. Non-empty means `cfg` claims a
    /// successor the branch does not actually enter at.
    pub interior_branch_targets: Vec<PcodeInsnAddr>,
    /// Sites whose answer nothing verified.
    ///
    /// Two shapes land here. A seated `Switch` holding exactly the caller's
    /// `known_targets` and nothing derived: not unresolved (the caller
    /// asserted the answer), but seating changes the CFG the classifier
    /// reads, so a stale seed can stop the selector deriving and take the
    /// site's real arms with it. And a site CONSUMED at CFG-build time,
    /// whether the answer came from a seed or from the classifier: a
    /// `LinkRegister` answer becomes a `Return` and a single target outside
    /// the function becomes a `TailCall`, leaving no placeholder and no
    /// anchor, so a dispatch that had more arms leaves no other trace.
    pub unverified_seeded_sites: Vec<PcodeInsnAddr>,
}

impl AnalyzeResult {
    /// Whether all four report channels are empty, i.e. the CFG carries no
    /// caveat at all.
    ///
    /// This is the question "may this result be incomplete?", which needs all
    /// four and which none of them answers alone. `false` is NOT always a
    /// loss: `unverified_seeded_sites` holds answers that are complete but
    /// that nothing verified, so a site consumed as a `Return` (an ARM
    /// `pop {pc}` dispatch, say) clears it. Read whichever channel is
    /// non-empty to tell the cases apart.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unresolved_indirect_branches.is_empty()
            && self.unverified_seeded_sites.is_empty()
            && self.isa_mode_conflicts.is_empty()
            && self.interior_branch_targets.is_empty()
    }
}

/// Doubles as a DEPTH limit: one iteration seats one LEVEL of indirect-branch
/// discovery, so a chain of trampolines each jumping to the next needs one
/// iteration per link. [`apply_resolutions`] reporting no change is the
/// terminator for everything shallower. Exhausting the cap is a RESULT, never
/// an error: the sites still growing are reported, and a site whose answer
/// never settles is abandoned and reported the same way.
const MAX_RESOLUTION_ITERATIONS: usize = 256;

/// `pipeline` running [`strider_opt::IndirectBranchClassify`], at most once: a
/// caller-supplied pipeline that already registers it would otherwise repeat
/// the pass's known-bits / dominator / value-range setup every re-lift round.
fn with_classify(mut pipeline: strider_opt::OptimizerPipeline) -> strider_opt::OptimizerPipeline {
    let classify = strider_opt::IndirectBranchClassify;
    if !pipeline
        .post_passes()
        .iter()
        .any(|pass| pass.name() == classify.name())
    {
        pipeline.add_post_pass(classify);
    }
    pipeline
}

/// One round's effect on the induced edge set.
#[derive(Default)]
struct Progress {
    /// Some address's successor set differs from the previous round's: the
    /// loop's progress signal.
    changed: bool,
    /// Addresses whose successor set strictly GREW.
    grew: Vec<PcodeInsnAddr>,
    /// Addresses that LOST a successor, so the signal is not monotone there.
    narrowed: Vec<PcodeInsnAddr>,
    /// Addresses whose fold dropped a successor the CLASSIFIER proved this
    /// round, or whose answer stopped being a superset. A seed says nothing
    /// about what the classifier derives, so this channel is never suppressed.
    /// A successor carried over from an earlier round needs no channel of its
    /// own: dropping one is exactly what stops the answer being a superset.
    derived_incomplete: Vec<PcodeInsnAddr>,
}

/// Fold `resolutions` into `known_targets`, keyed back to pcode addresses
/// via `unresolved`.
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
    caller_seed: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    unresolved: &UnresolvedAnchors,
    switch_anchors: &UnresolvedAnchors,
    resolutions: IndirectResolutions,
    flowing_isa_bit: &FxHashMap<PcodeInsnAddr, bool>,
    abandoned: &rustc_hash::FxHashSet<PcodeInsnAddr>,
) -> Result<Progress> {
    let node_to_addr: FxHashMap<strider_ir::node::NodeId, PcodeInsnAddr> = unresolved
        .iter()
        .chain(switch_anchors.iter())
        .map(|(addr, node)| (*node, *addr))
        .collect();
    // Staged per-address first so same-address collisions merge before
    // anything touches `known_targets`.
    // In the resolve loop every staged address came from `node_to_addr`, built
    // from the same anchors `flowing_isa_bits` mapped, so the lookup hits; the
    // default is for a caller that folds without one (the unit tests), where
    // `false` is the base ISA mode rather than a third rule for the bit.
    let flowing_at = |addr: PcodeInsnAddr| flowing_isa_bit.get(&addr).copied().unwrap_or(false);
    let mut staged: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    // Every successor the round's classifications name, before any mode filter:
    // what the fold has to account for. `None` is the `LinkRegister` successor,
    // which `ResolvedTargets` cannot hold alongside concrete targets, so a merge
    // that drops it has to be visible here.
    let mut reported: FxHashMap<PcodeInsnAddr, rustc_hash::FxHashSet<Option<u64>>> =
        FxHashMap::default();
    for (node, resolved) in resolutions {
        let Some(targets) = resolved else { continue };
        let addr = node_to_addr.get(&node).copied().ok_or_else(|| {
            anyhow!("classified IndirectBranch node {node:?} has no recorded pcode address")
        })?;
        // A site the loop gave up on takes no further classification: folding
        // one back in would re-enter the cycle that abandoned it.
        if abandoned.contains(&addr) {
            continue;
        }
        reported
            .entry(addr)
            .or_default()
            .extend(targets_of(&targets).map(|(target, _bit)| target));
        staged
            .entry(addr)
            .and_modify(|e| *e = merge_resolved(e, &targets, flowing_at(addr)))
            .or_insert(targets);
    }
    // A later classification REPLACES an earlier one for the same address, so a
    // set that narrows from unseatable to seatable once other branches resolve
    // is not stranded; convergence rides on the per-address diff, so an
    // unchanged cone re-inserts an equal value and reads as no growth. Every
    // successor the round reported yet did not seat comes back in
    // `Progress::derived_incomplete`.
    let mut progress = Progress::default();
    for (addr, targets) in staged {
        let prev = known_targets.get(&addr);
        // A seated `Switch` carries no ISA-mode input, so its re-derivation
        // reports no mode for ANY target, including the ones a mode-bearing
        // classification already proved. Re-deriving must widen the arm set,
        // not re-decode the old arms in the mode flowing into the branch.
        let targets = adopt_known_modes(prev, targets, flowing_at(addr));
        // The caller's own seed is unioned in by ADDRESS every round, after
        // both mode filters and never subject to them: a seed carries a mode
        // only if the caller built one, and filtering it against a mode-bearing
        // classification would delete the whole caller answer.
        let targets = match seed_for(caller_seed, addr) {
            Some(seed) => union_resolved(seed, &targets),
            None => targets,
        };
        let seated: rustc_hash::FxHashSet<Option<u64>> =
            targets_of(&targets).map(|(target, _bit)| target).collect();
        let derived_dropped = reported
            .get(&addr)
            .into_iter()
            .flatten()
            .any(|a| !seated.contains(a));
        let prev = prev.map(target_keys).unwrap_or_default();
        let next = target_keys(&targets);
        let narrowed = narrows(&prev, &next);
        if prev != next {
            progress.changed = true;
            if narrowed {
                progress.narrowed.push(addr);
            } else {
                progress.grew.push(addr);
            }
        }
        // A mode flip decodes an address two ways, so the seated arm is as
        // unclaimable as a dropped one.
        if derived_dropped || narrowed {
            progress.derived_incomplete.push(addr);
        }
        known_targets.insert(addr, targets);
    }
    Ok(progress)
}

/// The caller's answer for `addr`. A caller can only spell the machine address,
/// so a seed keyed at p-code index 0 counts for the whole instruction, matching
/// `CfgOptions::seated`.
fn seed_for(
    caller_seed: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    addr: PcodeInsnAddr,
) -> Option<&ResolvedTargets> {
    caller_seed
        .get(&addr)
        .or_else(|| caller_seed.get(&PcodeInsnAddr::at_machine_start(addr.machine_addr.addr)))
}

/// Whether `addr`'s settled successor set is still exactly what the caller
/// seeded, with nothing beyond it. An unseeded address is not covered.
///
/// A seed asserts the site is complete, and that assertion is what suppresses
/// the unclassifiable-`Switch` report. Outgrowing the seed disproves it: the
/// arms past the seed came from a classifier that has since stopped deriving,
/// so nothing vouches for the set that ended up seated.
fn seed_still_covers(
    caller_seed: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    settled: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    addr: PcodeInsnAddr,
) -> bool {
    let Some(seed) = seed_for(caller_seed, addr) else {
        return false;
    };
    let seeded: rustc_hash::FxHashSet<u64> =
        concrete_targets(seed).iter().map(|t| t.addr).collect();
    settled.get(&addr).is_none_or(|final_targets| {
        concrete_targets(final_targets)
            .iter()
            .all(|t| seeded.contains(&t.addr))
    })
}

/// `targets` with each mode-less entry taking the ISA mode `known` already
/// proved for that address.
///
/// At a site `known` proves a mode DIFFERENT from `flowing_isa_bit`, an
/// address only a mode-less re-derivation reports is dropped rather than
/// seated: `None` reads to the cfg as the mode flowing into the branch, which
/// at an interworking dispatch is the mode being switched away from. The
/// dropped set stays seatable, so the caller reports the site through
/// [`Progress::derived_incomplete`] rather than relying on the cfg to re-defer
/// it.
/// Where every proved mode IS the flowing one, seating `None` decodes
/// identically, so the set still widens.
fn adopt_known_modes(
    known: Option<&ResolvedTargets>,
    targets: ResolvedTargets,
    flowing_isa_bit: bool,
) -> ResolvedTargets {
    let Some(known) = known else { return targets };
    let known_targets = concrete_targets(known);
    let modes: FxHashMap<u64, bool> = known_targets
        .iter()
        .filter_map(|t| t.isa_bit.map(|bit| (t.addr, bit)))
        .collect();
    if modes.is_empty() || matches!(targets, ResolvedTargets::LinkRegister) {
        return targets;
    }
    let interworking = modes.values().any(|&bit| bit != flowing_isa_bit);
    let known_addrs: rustc_hash::FxHashSet<u64> = known_targets.iter().map(|t| t.addr).collect();
    let kept: Vec<strider_cfg::ResolvedTarget> = concrete_targets(&targets)
        .iter()
        .filter_map(|t| match (t.isa_bit, modes.get(&t.addr)) {
            (Some(_), _) => Some(*t),
            (None, Some(&bit)) => Some(strider_cfg::ResolvedTarget::new(t.addr, Some(bit))),
            (None, None) => (!interworking || known_addrs.contains(&t.addr)).then_some(*t),
        })
        .collect();
    resolved_from(kept)
}

/// A resolved target as the convergence check sees it: `(address, isa_bit)`,
/// with `None` address for `LinkRegister`.
type TargetKey = (Option<u64>, Option<bool>);

/// Whether one address's successor set lost ground between two rounds: the
/// ADDRESS projection is no longer a superset, or a target's proved mode flips
/// to the other ISA. A mode-less target taking a proved mode at an unchanged
/// address set is growth, and reading it as a loss would abandon a site that
/// was merely refining.
fn narrows(prev: &BTreeSet<TargetKey>, next: &BTreeSet<TargetKey>) -> bool {
    let addrs = |keys: &BTreeSet<TargetKey>| -> BTreeSet<Option<u64>> {
        keys.iter().map(|(addr, _bit)| *addr).collect()
    };
    if !addrs(next).is_superset(&addrs(prev)) {
        return true;
    }
    // A bit is a bool, so "next commits the other mode at this address" is a
    // point lookup.
    let next_bits: rustc_hash::FxHashSet<(Option<u64>, bool)> = next
        .iter()
        .filter_map(|(addr, bit)| bit.map(|b| (*addr, b)))
        .collect();
    prev.iter()
        .any(|(addr, bit)| bit.is_some_and(|prev_bit| next_bits.contains(&(*addr, !prev_bit))))
}

/// Successor set of a `ResolvedTargets`.
fn targets_of(r: &ResolvedTargets) -> impl Iterator<Item = TargetKey> + '_ {
    // `isa_bit` is in the key because it changes the DECODE: a target
    // reclassified from Thumb to ARM at one address would otherwise read as
    // converged, leaving regions decoded in the superseded mode.
    let (head, tail): (Option<TargetKey>, &[strider_cfg::ResolvedTarget]) = match r {
        ResolvedTargets::LinkRegister => (Some((None, None)), &[]),
        ResolvedTargets::Single(t) => (Some((Some(t.addr), t.isa_bit)), &[]),
        ResolvedTargets::Multiple(ts) => (None, ts.as_slice()),
    };
    head.into_iter()
        .chain(tail.iter().map(|t| (Some(t.addr), t.isa_bit)))
}

/// The concrete targets of a classification (empty for `LinkRegister`).
fn concrete_targets(r: &ResolvedTargets) -> &[strider_cfg::ResolvedTarget] {
    match r {
        ResolvedTargets::LinkRegister => &[],
        ResolvedTargets::Single(t) => std::slice::from_ref(t),
        ResolvedTargets::Multiple(ts) => ts.as_slice(),
    }
}

/// Union two classifications of one pcode address that BOTH derive from the
/// IR, dropping a mode-less address only the non-interworking side knows: a
/// `Switch` re-derivation carries no ISA-mode input, so every address it
/// discovers reports `None`, which the cfg decodes in `flowing_isa_bit`. That
/// is a guess only against a side proving a DIFFERENT mode.
fn merge_resolved(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: bool,
) -> ResolvedTargets {
    combine_resolved(a, b, Some(flowing_isa_bit))
}

/// [`merge_resolved`] by plain address union, for a caller's `known_targets`
/// seed. The mode filter would delete a mode-less seed outright whenever the
/// classifier proves a mode, so the seed is unioned without it.
fn union_resolved(a: &ResolvedTargets, b: &ResolvedTargets) -> ResolvedTargets {
    combine_resolved(a, b, None)
}

/// Two `LinkRegister`s stay `LinkRegister`; anything else widens to `Single` or
/// `Multiple` over the unioned targets, deduped by address. `flowing_isa_bit`
/// is the mode flowing into the branch, `None` disabling the interworking
/// drop. Order-independent: two entries for one address keep the ISA mode only
/// where they agree, since a bit contradicted by the other classification
/// cannot be trusted (see [`merge_isa_bit`]).
///
/// `ResolvedTargets` cannot hold "returns AND jumps to X", so mixing
/// `LinkRegister` with a concrete set LOSES the return successor.
/// [`apply_resolutions`] accounts for it: `None` is one of the successors
/// `reported` tracks, so the site comes back in [`Progress::derived_incomplete`]
/// instead of converging on the smaller answer.
fn combine_resolved(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: Option<bool>,
) -> ResolvedTargets {
    if matches!(a, ResolvedTargets::LinkRegister) && matches!(b, ResolvedTargets::LinkRegister) {
        return ResolvedTargets::LinkRegister;
    }
    let a_targets = concrete_targets(a);
    let b_targets = concrete_targets(b);
    let interworks = |side: &[strider_cfg::ResolvedTarget]| {
        flowing_isa_bit.is_some_and(|flowing| {
            side.iter()
                .any(|t| t.isa_bit.is_some_and(|bit| bit != flowing))
        })
    };
    let a_interworks = interworks(a_targets);
    let b_interworks = interworks(b_targets);
    let addrs = |side: &[strider_cfg::ResolvedTarget]| -> rustc_hash::FxHashSet<u64> {
        side.iter().map(|t| t.addr).collect()
    };
    let a_addrs = addrs(a_targets);
    let b_addrs = addrs(b_targets);
    let guessed = |t: &strider_cfg::ResolvedTarget,
                   own_interworks: bool,
                   other_interworks: bool,
                   other_addrs: &rustc_hash::FxHashSet<u64>| {
        other_interworks && !own_interworks && t.isa_bit.is_none() && !other_addrs.contains(&t.addr)
    };
    let mut targets: Vec<strider_cfg::ResolvedTarget> = a_targets
        .iter()
        .filter(|t| !guessed(t, a_interworks, b_interworks, &b_addrs))
        .chain(
            b_targets
                .iter()
                .filter(|t| !guessed(t, b_interworks, a_interworks, &a_addrs)),
        )
        .copied()
        .collect();
    targets.sort_by_key(|t| t.addr);
    let mut contradicted: rustc_hash::FxHashSet<u64> = rustc_hash::FxHashSet::default();
    targets.dedup_by(|later, kept| {
        if later.addr != kept.addr {
            return false;
        }
        match merge_isa_bit(kept.isa_bit, later.isa_bit) {
            Some(bit) => kept.isa_bit = bit,
            // Two classifications proved DIFFERENT modes for one address.
            // Decoding it once in either is a coin flip, and the decode-once
            // CFG cannot hold both, so drop it; [`apply_resolutions`] reports
            // the site, which stays seatable on its remaining arms.
            None => {
                contradicted.insert(kept.addr);
            }
        }
        true
    });
    targets.retain(|t| !contradicted.contains(&t.addr));
    resolved_from(targets)
}

/// `targets` as a [`ResolvedTargets`], and the ONE place an EMPTY one is built.
///
/// `strider_cfg` documents `Multiple` as non-empty and defends: it seats no
/// `Switch` for an empty set, re-emitting the placeholder, so the site comes
/// back as a live unresolved branch. That is the honest answer once every
/// target has been contradicted (the enum spells no "nothing trustworthy
/// here") where staging nothing would leave the previous round's answer
/// seated as if it had never been contradicted.
fn resolved_from(targets: Vec<strider_cfg::ResolvedTarget>) -> ResolvedTargets {
    match targets.as_slice() {
        [single] => ResolvedTargets::Single(*single),
        _ => ResolvedTargets::Multiple(targets),
    }
}

/// The ISA mode two classifications of one target agree on, or `None` when they
/// CONTRADICT each other, which is not the same as neither committing one: a
/// side committing no mode reports nothing rather than denying the other's, so
/// a mode-less `Switch` re-derivation cannot erase a seated interworking mode.
/// Commutative, so `merge_resolved` reads the same either way round.
fn merge_isa_bit(a: Option<bool>, b: Option<bool>) -> Option<Option<bool>> {
    match (a, b) {
        (Some(x), Some(y)) if x != y => None,
        (x, y) => Some(x.or(y)),
    }
}

/// Abandons every site naming a target `cfg` could not trust, returning those
/// sites so the caller can report them.
///
/// Two ways a target goes bad. A classifier that over-approximates a jump-table
/// bound reaches past the table and names addresses that are not code; decoding
/// one is a failure OF THAT TARGET, not of the function. And an address two
/// edges reach in different ISA modes is decoded once, in whichever mode won
/// the work queue, so the other edge's arm is not the instruction stream it
/// believes. Either way, dropping the site keeps the rest of the function
/// analysable and reporting it is what stops the answer being silently wrong.
fn abandon_undecodable(
    cfg: &strider_cfg::Cfg,
    abandoned: &mut rustc_hash::FxHashSet<PcodeInsnAddr>,
    known_targets: &mut FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Vec<PcodeInsnAddr> {
    let bad: rustc_hash::FxHashSet<u64> = cfg
        .undecodable_seeded_targets()
        .iter()
        .chain(cfg.isa_mode_conflicts())
        .map(|a| a.machine_addr.addr)
        .collect();
    if bad.is_empty() {
        return Vec::new();
    }
    let hit: Vec<PcodeInsnAddr> = known_targets
        .iter()
        .filter(|(_, targets)| match targets {
            ResolvedTargets::LinkRegister => false,
            ResolvedTargets::Single(t) => bad.contains(&t.addr),
            ResolvedTargets::Multiple(ts) => ts.iter().any(|t| bad.contains(&t.addr)),
        })
        .map(|(site, _)| *site)
        .collect();
    for site in &hit {
        known_targets.remove(site);
        abandoned.insert(*site);
    }
    hit
}

/// Seated `Switch` sites whose arms are exactly what the caller seeded and
/// nothing more, with the classifier silent about them.
///
/// Seating a seed changes the CFG the classifier reads, so a stale or wrong
/// seed can stop the selector deriving and then suppress the report of the arms
/// that cost. Such a site is not "unresolved" (the caller asserted the
/// answer), but nothing verified it, so it is named separately.
fn unverified_seeded_sites(
    cfg: &strider_cfg::Cfg,
    switch_anchors: &UnresolvedAnchors,
    unclassified: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    caller_seed: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    settled: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Vec<PcodeInsnAddr> {
    let mut out: Vec<PcodeInsnAddr> = switch_anchors
        .iter()
        .filter(|(addr, node)| {
            if !unclassified.contains(node) {
                return false;
            }
            // Anything beyond the seed means the classifier did contribute,
            // and then the site is reported as unresolved instead.
            seed_still_covers(caller_seed, settled, *addr)
        })
        .map(|(addr, _node)| *addr)
        .collect();
    // A `LinkRegister` seed is consumed at CFG-build time: it becomes a
    // `Return`, leaving no placeholder and no `Switch` anchor, so the filter
    // above cannot see it and no other channel names it either. The seat is
    // the only record that a caller's answer replaced whatever the classifier
    // would have derived.
    out.extend_from_slice(cfg.link_register_seated());
    // A `TailCall` seat consumes the site the same way: an answer of one
    // target outside the function replaces the dispatch, and any other arm it
    // had is gone with no placeholder and no anchor left behind.
    out.extend_from_slice(cfg.tail_call_seated());
    out.sort_unstable();
    out.dedup();
    out
}

/// Sites whose seated `Switch` holds FEWER arms than `known_targets` asked for.
///
/// The cfg drops a target it cannot express as an arm (one interior to a
/// region but off every instruction boundary, which it only discovers once the
/// sibling arm it lands in has been decoded) and leaves the rest of the table
/// seated. The fold cannot see that: it compares its own result against the
/// previous round, so the loss reads as convergence. Comparing against the CFG
/// the round actually built is what keeps a converged answer honest.
fn seated_arm_losses(
    cfg: &strider_cfg::Cfg,
    known_targets: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
) -> Vec<PcodeInsnAddr> {
    let mut out = Vec::new();
    for region in cfg.regions() {
        let strider_cfg::RegionTerminator::Switch { targets, addr, .. } = &region.terminator else {
            continue;
        };
        let Some(asked) = known_targets.get(addr) else {
            continue;
        };
        let seated: rustc_hash::FxHashSet<u64> = targets.iter().map(|t| t.addr).collect();
        if concrete_targets(asked)
            .iter()
            .any(|t| !seated.contains(&t.addr))
        {
            out.push(*addr);
        }
    }
    out
}

/// The two non-placeholder channels `live_unresolved_branches` merges.
#[derive(Clone, Copy)]
struct DerivedChannels<'a> {
    /// Sites still growing when the iteration cap ran out.
    budget_exhausted: &'a [PcodeInsnAddr],
    /// Sites whose fold dropped a successor the classifier proved, whose answer
    /// stopped being a superset, or that the loop abandoned.
    incomplete_derived: &'a [PcodeInsnAddr],
}

/// Sites the analysis cannot claim a complete successor set for (sorted,
/// deduplicated):
///
/// - a lift-time deferred site whose placeholder is still live. Classification
///   alone does not exclude one: the cfg layer re-emits the placeholder for a
///   target set it cannot seat, and `function` then still holds an
///   `IndirectBranch`.
/// - a seated `Switch` whose selector no longer derives (`unclassified`). Its
///   arms are whatever the round that seated them proved, which may be a proper
///   SUBSET, and seating consumed the placeholder, so this list is its only
///   report.
/// - `budget_exhausted`, the sites still growing when the iteration cap ran out.
/// - `incomplete_derived`, the sites whose fold dropped a successor the
///   classifier proved, whose answer stopped being a superset, or that the loop
///   abandoned.
///
/// A caller seed asserts the site is complete, which suppresses the SECOND of
/// those, the unclassified seated `Switch`, but only while `settled`, the
/// site's final successor set, holds nothing the seed did not name. A site that
/// OUTGREW its seed disproves the assertion, and the arms past the seed came
/// from a classifier that has since gone silent, so nothing vouches for them.
/// [`unverified_seeded_sites`] applies the same test the other way round, so a
/// seeded unclassifiable site lands in exactly one of the two reports. The seed
/// does NOT suppress a live placeholder: the cfg re-emits one exactly when it
/// could not seat the seeded set (empty, or a target out of range or interior
/// to an instruction), so the seed did not in fact settle that site. It does
/// NOT suppress `budget_exhausted`: outgrowing the seed is itself evidence the
/// seed was incomplete. And it does NOT suppress `incomplete_derived`: a seed
/// is an answer about the site, not a licence to drop what the classifier
/// proves about it.
fn live_unresolved_branches(
    live_indirect: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    unresolved: &UnresolvedAnchors,
    switch_anchors: &UnresolvedAnchors,
    unclassified: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    caller_seed: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    settled: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    derived: DerivedChannels<'_>,
) -> Vec<PcodeInsnAddr> {
    // `live_indirect` is the classifier's key set, snapshotted from the
    // `build_lift` that produced `function`: every reachable placeholder AND
    // seated `Switch`. Intersecting it with `unresolved`, which anchors only
    // placeholders, selects the live ones.
    let mut out: Vec<PcodeInsnAddr> = unresolved
        .iter()
        .filter(|(_addr, node)| live_indirect.contains(node))
        .map(|(addr, _node)| *addr)
        .collect();
    out.extend(
        switch_anchors
            .iter()
            .filter(|(addr, node)| {
                unclassified.contains(node) && !seed_still_covers(caller_seed, settled, *addr)
            })
            .map(|(addr, _node)| *addr),
    );
    // Neither is seed-suppressed: a seed cannot vouch for a site the classifier
    // was still GROWING when the budget ran out, nor for one whose derived
    // answer lost ground.
    out.extend(derived.budget_exhausted.iter().copied());
    out.extend(derived.incomplete_derived);
    out.sort_unstable();
    out.dedup();
    out
}

/// The sites the classifier could not derive this round.
fn unclassified_nodes(
    resolutions: &IndirectResolutions,
) -> rustc_hash::FxHashSet<strider_ir::node::NodeId> {
    resolutions
        .iter()
        .filter(|(_node, resolved)| resolved.is_none())
        .map(|(node, _resolved)| *node)
        .collect()
}

/// Each deferred `BranchIndirect`'s pcode address paired with the `NodeId`
/// of the `IndirectBranch` placeholder lifted for it.
type UnresolvedAnchors = Vec<(PcodeInsnAddr, strider_ir::node::NodeId)>;

/// `None` = unresolvable this iteration.
type IndirectResolutions = FxHashMap<strider_ir::node::NodeId, Option<ResolvedTargets>>;

/// One address's induced successor set, the unit the convergence check
/// compares. `target = None` is a `LinkRegister` resolution.
fn target_keys(resolved: &ResolvedTargets) -> BTreeSet<TargetKey> {
    targets_of(resolved).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_cfg::MachineInsnAddr;

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(machine),
            insn_index: 0,
        }
    }

    #[test]
    fn target_keys_of_link_register_is_the_unknown_successor() {
        let keys = target_keys(&ResolvedTargets::LinkRegister);
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&(None, None)));
    }

    #[test]
    fn target_keys_of_single_resolution_is_one_successor() {
        let expected: BTreeSet<TargetKey> = std::iter::once((Some(0x2000), None)).collect();
        assert_eq!(
            target_keys(&ResolvedTargets::Single(0x2000.into())),
            expected
        );
    }

    #[test]
    fn target_keys_of_multiple_resolution_is_n_successors() {
        let keys = target_keys(&ResolvedTargets::Multiple(
            vec![0x2000, 0x3000, 0x4000]
                .into_iter()
                .map(Into::into)
                .collect(),
        ));
        assert_eq!(keys.len(), 3);
    }

    /// A set, so a repeated target is one successor and the comparison the
    /// convergence check makes is order-independent.
    #[test]
    fn target_keys_dedups_duplicate_targets_in_multiple() {
        let keys = target_keys(&ResolvedTargets::Multiple(
            vec![0x2000, 0x2000, 0x2000]
                .into_iter()
                .map(Into::into)
                .collect(),
        ));
        assert_eq!(keys.len(), 1);
        let reversed = target_keys(&ResolvedTargets::Multiple(
            vec![0x3000, 0x2000].into_iter().map(Into::into).collect(),
        ));
        let forward = target_keys(&ResolvedTargets::Multiple(
            vec![0x2000, 0x3000].into_iter().map(Into::into).collect(),
        ));
        assert_eq!(forward, reversed);
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

    /// [`live_unresolved_branches`] with no `Switch` anchors and no caller
    /// seed: the placeholder half on its own.
    fn live_placeholders(
        live_indirect: &rustc_hash::FxHashSet<NodeId>,
        unresolved: &UnresolvedAnchors,
    ) -> Vec<PcodeInsnAddr> {
        live_unresolved_branches(
            live_indirect,
            unresolved,
            &Vec::new(),
            &rustc_hash::FxHashSet::default(),
            &FxHashMap::default(),
            &FxHashMap::default(),
            DerivedChannels {
                budget_exhausted: &[],
                incomplete_derived: &[],
            },
        )
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
        let live = live_indirect_set(&function);
        assert_eq!(live_placeholders(&live, &unresolved), vec![addr]);
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
        let live = live_indirect_set(&function);
        assert!(
            live_placeholders(&live, &unresolved).is_empty(),
            "a dead / non-live IndirectBranch placeholder must not be reported"
        );
    }

    #[test]
    fn live_unresolved_reports_classified_but_unseated_branch() {
        // The report key is the LIVE placeholder, not the site's
        // classification state: the cfg layer re-emits a placeholder for a
        // target set it could not seat, and that site must still be named.
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let live = live_indirect_set(&function);
        assert_eq!(
            live_placeholders(&live, &unresolved),
            vec![addr],
            "a live placeholder must be reported even once its site is classified"
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
        known.insert(
            addr,
            ResolvedTargets::Multiple(vec![0x2000, 0x3000].into_iter().map(Into::into).collect()),
        );
        let mut same: IndirectResolutions = FxHashMap::default();
        same.insert(
            node,
            Some(ResolvedTargets::Multiple(
                vec![0x2000, 0x3000].into_iter().map(Into::into).collect(),
            )),
        );
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            same,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert!(
            !progress.changed,
            "identical re-classification must not report growth"
        );
        assert_eq!(
            known[&addr],
            ResolvedTargets::Multiple(vec![0x2000, 0x3000].into_iter().map(Into::into).collect())
        );

        let mut improved: IndirectResolutions = FxHashMap::default();
        improved.insert(
            node,
            Some(ResolvedTargets::Multiple(
                vec![0x2000, 0x2004].into_iter().map(Into::into).collect(),
            )),
        );
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            improved,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert!(
            progress.changed,
            "an improved classification must be applied"
        );
        assert!(
            !progress.narrowed.is_empty(),
            "dropping 0x3000 for 0x2004 is not monotone discovery"
        );
        assert_eq!(
            known[&addr],
            ResolvedTargets::Multiple(vec![0x2000, 0x2004].into_iter().map(Into::into).collect())
        );
    }

    #[test]
    fn apply_resolutions_first_classification_grows() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(ResolvedTargets::Single(0x2000.into())));
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert!(
            progress.changed,
            "a first-time classification must register as growth"
        );
        assert_eq!(progress.grew, vec![addr]);
        assert!(progress.narrowed.is_empty());
        assert_eq!(known[&addr], ResolvedTargets::Single(0x2000.into()));
    }

    #[test]
    fn merge_resolved_unions_multiple_targets() {
        let merged = merge_resolved(
            &ResolvedTargets::Multiple(vec![0x1000, 0x2000].into_iter().map(Into::into).collect()),
            &ResolvedTargets::Multiple(vec![0x2000, 0x3000].into_iter().map(Into::into).collect()),
            false,
        );
        assert_eq!(
            merged,
            ResolvedTargets::Multiple(
                vec![0x1000, 0x2000, 0x3000]
                    .into_iter()
                    .map(Into::into)
                    .collect()
            )
        );
    }

    #[test]
    fn merge_resolved_two_link_registers_stay_link_register() {
        assert_eq!(
            merge_resolved(
                &ResolvedTargets::LinkRegister,
                &ResolvedTargets::LinkRegister,
                false,
            ),
            ResolvedTargets::LinkRegister
        );
    }

    #[test]
    fn merge_resolved_single_plus_single_widens_to_multiple() {
        assert_eq!(
            merge_resolved(
                &ResolvedTargets::Single(0x1000.into()),
                &ResolvedTargets::Single(0x2000.into()),
                false,
            ),
            ResolvedTargets::Multiple(vec![0x1000, 0x2000].into_iter().map(Into::into).collect())
        );
    }

    /// Two placeholders at one pcode address classifying the SAME target with
    /// OPPOSITE ISA modes. Each address decodes once, so every mode the merge
    /// could pick has been disproved by one side, and "no committed
    /// mode" is not neutral: the cfg reads it as the mode flowing into the
    /// branch, which on an interworking `bx` is the one being switched away
    /// from. The address is dropped instead, leaving the empty [`resolved_from`]
    /// builds, so the site fails the cfg's seatable guard and is reported
    /// unresolved. Order-independent.
    #[test]
    fn merge_resolved_drops_a_target_two_classifications_disagree_on() {
        let thumb = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true)));
        let arm = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(false)));
        let empty = ResolvedTargets::Multiple(vec![]);
        assert_eq!(merge_resolved(&thumb, &arm, false), empty);
        assert_eq!(
            merge_resolved(&arm, &thumb, false),
            empty,
            "order-independent"
        );
    }

    /// The single place an EMPTY `Multiple` is built. `strider_cfg` documents
    /// that value as unseatable, which is the answer wanted here: the cfg
    /// re-emits the placeholder and the site is reported unresolved.
    #[test]
    fn resolved_from_builds_the_unseatable_empty_multiple() {
        assert_eq!(resolved_from(Vec::new()), ResolvedTargets::Multiple(vec![]));
        assert_eq!(
            resolved_from(vec![0x2000.into()]),
            ResolvedTargets::Single(0x2000.into()),
        );
    }

    /// Two anchors at ONE pcode address in one round, one of them a `Switch`
    /// re-derivation, which carries no ISA-mode input and so reports `None` for
    /// every address it discovers. The cfg reads that as "inherit the branch's
    /// mode", a guess on an interworking dispatch, so the mode-less side does
    /// not contribute addresses of its own. Across ROUNDS the seated set is not
    /// merged but re-derived, and widening there is
    /// [`adopt_known_modes`]'s job.
    #[test]
    fn merge_resolved_does_not_widen_a_mode_bearing_set_with_mode_less_arms() {
        let seated =
            ResolvedTargets::Multiple(vec![strider_cfg::ResolvedTarget::new(0x2000, Some(true))]);
        let rederived = ResolvedTargets::Multiple(vec![
            strider_cfg::ResolvedTarget::new(0x2000, None),
            strider_cfg::ResolvedTarget::new(0x3000, None),
        ]);
        let merged = merge_resolved(&seated, &rederived, false);
        assert_eq!(
            concrete_targets(&merged),
            &[strider_cfg::ResolvedTarget::new(0x2000, Some(true))],
            "the mode-less 0x3000 must not be seated as an arm",
        );
        assert_eq!(
            concrete_targets(&merge_resolved(&rederived, &seated, false)),
            concrete_targets(&merged),
            "order-independent",
        );
    }

    /// A classification that commits NO mode does not contradict one that
    /// does; conflating the two erases a seated interworking mode on the next
    /// resolution round.
    #[test]
    fn merge_resolved_keeps_a_mode_the_other_side_does_not_report() {
        let thumb = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true)));
        let no_mode = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, None));
        assert_eq!(merge_resolved(&thumb, &no_mode, false), thumb);
        assert_eq!(
            merge_resolved(&no_mode, &thumb, false),
            thumb,
            "order-independent"
        );
    }

    /// Agreeing bits are kept: the collapse above must not throw away a mode
    /// both classifications assert.
    #[test]
    fn merge_resolved_agreeing_isa_bits_are_kept() {
        let a = ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true)));
        let b = ResolvedTargets::Multiple(vec![
            strider_cfg::ResolvedTarget::new(0x2000, Some(true)),
            strider_cfg::ResolvedTarget::new(0x3000, None),
        ]);
        assert_eq!(
            merge_resolved(&a, &b, false),
            ResolvedTargets::Multiple(vec![
                strider_cfg::ResolvedTarget::new(0x2000, Some(true)),
                strider_cfg::ResolvedTarget::new(0x3000, None),
            ])
        );
    }

    /// A side proving only the FLOWING mode seats a mode-less arm
    /// identically, so a mode-less classification still widens it: the drop is
    /// for sides that switch ISA.
    #[test]
    fn merge_resolved_widens_a_set_whose_proved_mode_is_the_flowing_one() {
        use strider_cfg::ResolvedTarget;
        let seated = ResolvedTargets::Multiple(vec![ResolvedTarget::new(0x2000, Some(false))]);
        let rederived = ResolvedTargets::Multiple(vec![
            ResolvedTarget::new(0x2000, None),
            ResolvedTarget::new(0x3000, None),
        ]);
        assert_eq!(
            concrete_targets(&merge_resolved(&seated, &rederived, false)),
            &[
                ResolvedTarget::new(0x2000, Some(false)),
                ResolvedTarget::new(0x3000, None),
            ],
        );
    }

    /// Two classifications of one address, each proving a mode the other
    /// contradicts on every arm.
    #[test]
    fn merge_resolved_does_not_go_quadratic_in_arm_count() {
        use strider_cfg::ResolvedTarget;
        const ARMS: u64 = 20_000;
        let thumb = ResolvedTargets::Multiple(
            (0..ARMS)
                .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true)))
                .collect(),
        );
        let arm = ResolvedTargets::Multiple(
            (0..ARMS)
                .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(false)))
                .collect(),
        );
        let start = std::time::Instant::now();
        let merged = merge_resolved(&thumb, &arm, false);
        let elapsed = start.elapsed();
        assert!(concrete_targets(&merged).is_empty());
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "merging {ARMS} contradicted arms took {elapsed:?}: the drop is \
             quadratic in arms",
        );
    }

    /// Scales ARMS at one site, where
    /// [`apply_resolutions_does_not_go_quadratic_in_accumulated_targets`]
    /// scales addresses: a mode-less re-derivation of an interworking table
    /// checks every new arm against the seated set.
    #[test]
    fn apply_resolutions_does_not_go_quadratic_in_arms_at_one_site() {
        use strider_cfg::ResolvedTarget;
        const ARMS: u64 = 20_000;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(
            addr,
            ResolvedTargets::Multiple(
                (0..ARMS)
                    .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true)))
                    .collect(),
            ),
        );
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(
            node,
            Some(ResolvedTargets::Multiple(
                (0..ARMS * 2)
                    .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, None))
                    .collect(),
            )),
        );

        let start = std::time::Instant::now();
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        let elapsed = start.elapsed();

        assert_eq!(progress.derived_incomplete, vec![addr]);
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "folding {ARMS} seated arms against {} re-derived ones took \
             {elapsed:?}: the per-site fold is quadratic in arms",
            ARMS * 2,
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
        resolutions.insert(indirect, Some(ResolvedTargets::Single(0x2000.into())));
        resolutions.insert(other, Some(ResolvedTargets::Single(0x3000.into())));
        apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            known[&addr],
            ResolvedTargets::Multiple(vec![0x2000, 0x3000].into_iter().map(Into::into).collect()),
            "two same-addr classifications must be merged, not overwritten"
        );
    }

    /// `ResolvedTargets` cannot hold "returns AND jumps to X", so merging a
    /// `LinkRegister` classification with a concrete one at the same address
    /// loses the return successor. It must not read as clean growth.
    #[test]
    fn apply_resolutions_reports_a_dropped_link_register_successor() {
        use strider_ir::node::NodeKind;
        use strider_ir::{IRViewer, IRWalker};
        let (function, indirect) = fn_with_live_indirect_branch();
        let other = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .expect("const node");
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, indirect), (addr, other)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(indirect, Some(ResolvedTargets::LinkRegister));
        resolutions.insert(other, Some(ResolvedTargets::Single(0x2000.into())));
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            known[&addr],
            ResolvedTargets::Single(0x2000.into()),
            "the concrete side is what the cfg can seat"
        );
        assert_eq!(
            progress.derived_incomplete,
            vec![addr],
            "the return successor vanished from the seated answer unreported"
        );
    }

    /// Merging one classification must not touch the targets already
    /// accumulated for every other address. The ceiling is a blowup guard, not
    /// a pin on the linear cost: it clears by three orders of magnitude.
    #[test]
    fn apply_resolutions_does_not_go_quadratic_in_accumulated_targets() {
        let (_function, node) = fn_with_live_indirect_branch();
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        for i in 0..200_000u64 {
            known.insert(
                pcode_addr(0x10_0000 + i * 4),
                ResolvedTargets::Single((0x20_0000 + i * 4).into()),
            );
        }
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(ResolvedTargets::Single(0x2000.into())));

        let start = std::time::Instant::now();
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &unresolved,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        let elapsed = start.elapsed();

        assert!(progress.changed);
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "merging one resolution into {} accumulated targets took {elapsed:?}: \
             the whole induced edge set is being rebuilt",
            known.len(),
        );
    }

    /// Round 1 seats an interworking table with a proven per-target mode.
    /// Round 2 re-derives the now-seated `Switch`, which carries no ISA-mode
    /// input, so every target comes back mode-less. The seated modes must
    /// survive the fold, or the arms decode in the superseded ISA, and an
    /// address only the mode-less side knows must not be seated: the cfg would
    /// decode it in the mode flowing into the branch, the same guess
    /// [`merge_resolved`] refuses to make within a round.
    #[test]
    fn apply_resolutions_keeps_a_seated_mode_a_rederivation_does_not_report() {
        use strider_cfg::ResolvedTarget;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(
            addr,
            ResolvedTargets::Multiple(vec![
                ResolvedTarget::new(0x2000, Some(true)),
                ResolvedTarget::new(0x2004, Some(true)),
            ]),
        );
        let mut rederived: IndirectResolutions = FxHashMap::default();
        rederived.insert(
            node,
            Some(ResolvedTargets::Multiple(vec![
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x2004, None),
                ResolvedTarget::new(0x2008, None),
            ])),
        );
        apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &Vec::new(),
            &anchors,
            rederived,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            concrete_targets(&known[&addr]),
            &[
                ResolvedTarget::new(0x2000, Some(true)),
                ResolvedTarget::new(0x2004, Some(true)),
            ],
            "a mode-less re-derivation must adopt the modes already seated, and \
             must not seat 0x2008 with an inherited mode",
        );
    }

    /// The drop is scoped to sites with a proved mode: a table nothing ever
    /// proved a mode for still widens on re-derivation.
    #[test]
    fn apply_resolutions_widens_a_mode_less_site_on_rederivation() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert!(progress.changed);
        assert_eq!(folded, multiple(&[(0x2000, None), (0x2004, None)]));
    }

    /// A proved mode EQUAL to the flowing one seats identically to no mode at
    /// all, so the set still widens: the drop is for sites that switch ISA.
    #[test]
    fn apply_resolutions_widens_a_site_whose_proved_mode_is_the_flowing_one() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, Some(false))])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert!(progress.changed);
        assert_eq!(folded, multiple(&[(0x2000, Some(false)), (0x2004, None)]));
    }

    /// An address the seated set already holds without a proved mode stays;
    /// only addresses NEW to a mode-bearing site are dropped.
    #[test]
    fn apply_resolutions_keeps_a_known_mode_less_arm_but_drops_a_new_one() {
        let (_progress, folded) = round(
            Some(multiple(&[(0x2000, Some(true)), (0x3000, None)])),
            multiple(&[(0x2000, None), (0x3000, None), (0x4000, None)]),
            None,
        );
        assert_eq!(folded, multiple(&[(0x2000, Some(true)), (0x3000, None)]));
    }

    /// The caller's seed is mode-less by construction, so a classification that
    /// proves a mode must not delete it.
    #[test]
    fn apply_resolutions_keeps_the_caller_seed_against_a_mode_bearing_answer() {
        use strider_cfg::ResolvedTarget;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        let mut seed: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        seed.insert(
            addr,
            ResolvedTargets::Multiple(vec![
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x3000, None),
            ]),
        );
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(
            node,
            Some(ResolvedTargets::Single(ResolvedTarget::new(
                0x4000,
                Some(true),
            ))),
        );
        apply_resolutions(
            &mut known,
            &seed,
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        assert_eq!(
            concrete_targets(&known[&addr]),
            &[
                ResolvedTarget::new(0x2000, None),
                ResolvedTarget::new(0x3000, None),
                ResolvedTarget::new(0x4000, Some(true)),
            ],
            "seeding may only ADD edges, whatever the classifier proves about the mode",
        );
    }

    /// A seated `Switch` the classifier can no longer derive keeps its stale,
    /// possibly PARTIAL arm set. Seating consumed its placeholder, so the site
    /// must come back through its anchor or the caller reads
    /// `unresolved == []` next to a table missing arms.
    #[test]
    fn unclassifiable_seated_switch_is_reported_unresolved() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let switch_anchors: UnresolvedAnchors = vec![(addr, node)];
        let unclassified: rustc_hash::FxHashSet<NodeId> = std::iter::once(node).collect();
        assert_eq!(
            live_unresolved_branches(
                &rustc_hash::FxHashSet::default(),
                &Vec::new(),
                &switch_anchors,
                &unclassified,
                &FxHashMap::default(),
                &FxHashMap::default(),
                DerivedChannels {
                    budget_exhausted: &[],
                    incomplete_derived: &[],
                },
            ),
            vec![addr],
            "a seated Switch with no derivable arm set must be reported",
        );
    }

    /// A caller seed asserts the site is complete, so the same unclassifiable
    /// `Switch` is the caller's answer, not a gap, while the settled set is
    /// still the seed and nothing else.
    #[test]
    fn caller_seeded_switch_is_not_reported_unresolved() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let switch_anchors: UnresolvedAnchors = vec![(addr, node)];
        let unclassified: rustc_hash::FxHashSet<NodeId> = std::iter::once(node).collect();
        let mut seed: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        seed.insert(addr, ResolvedTargets::Single(0x2000.into()));
        let mut settled: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        settled.insert(addr, ResolvedTargets::Single(0x2000.into()));
        assert!(
            live_unresolved_branches(
                &rustc_hash::FxHashSet::default(),
                &Vec::new(),
                &switch_anchors,
                &unclassified,
                &seed,
                &settled,
                DerivedChannels {
                    budget_exhausted: &[],
                    incomplete_derived: &[],
                },
            )
            .is_empty(),
        );
    }

    /// A site that OUTGREW its seed and then stopped classifying. The seed
    /// asserted a set the site has since grown past, so it vouches for none of
    /// the arms beyond it, and the round that proved them is gone: the arms are
    /// whatever the last deriving round seated, which may be a proper subset.
    /// `unverified_seeded_sites` stays silent here (the settled set is not the
    /// seed), so this is the site's only report.
    #[test]
    fn a_switch_that_outgrew_its_seed_is_reported_unresolved() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let switch_anchors: UnresolvedAnchors = vec![(addr, node)];
        let unclassified: rustc_hash::FxHashSet<NodeId> = std::iter::once(node).collect();
        let mut seed: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        seed.insert(addr, ResolvedTargets::Single(0x2000.into()));
        let mut settled: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        settled.insert(
            addr,
            ResolvedTargets::Multiple(vec![0x2000, 0x3000].into_iter().map(Into::into).collect()),
        );
        assert_eq!(
            live_unresolved_branches(
                &rustc_hash::FxHashSet::default(),
                &Vec::new(),
                &switch_anchors,
                &unclassified,
                &seed,
                &settled,
                DerivedChannels {
                    budget_exhausted: &[],
                    incomplete_derived: &[],
                },
            ),
            vec![addr],
            "the seed cannot vouch for the arm the site grew past it",
        );
        assert!(
            !seed_still_covers(&seed, &settled, addr),
            "the shared test `unverified_seeded_sites` reads the other way \
             round must agree, or the site lands in both reports or neither",
        );
    }

    #[test]
    fn classify_post_pass_is_registered_exactly_once() {
        let mut already = strider_opt::OptimizerPipeline::new();
        already.add_post_pass(strider_opt::IndirectBranchClassify);
        assert_eq!(with_classify(already).post_passes().len(), 1);
        assert_eq!(
            with_classify(strider_opt::OptimizerPipeline::new())
                .post_passes()
                .len(),
            1,
        );
    }

    /// One [`apply_resolutions`] round at one address: `known` holding `prev`,
    /// the classifier reporting `next`, no caller seed unless `seed`.
    fn round(
        prev: Option<ResolvedTargets>,
        next: ResolvedTargets,
        seed: Option<ResolvedTargets>,
    ) -> (Progress, ResolvedTargets) {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        if let Some(prev) = prev {
            known.insert(addr, prev);
        }
        let mut seeds: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        if let Some(seed) = seed {
            seeds.insert(addr, seed);
        }
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(next));
        let progress = apply_resolutions(
            &mut known,
            &seeds,
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("apply");
        (progress, known.remove(&addr).expect("folded"))
    }

    fn multiple(targets: &[(u64, Option<bool>)]) -> ResolvedTargets {
        ResolvedTargets::Multiple(
            targets
                .iter()
                .map(|&(addr, bit)| strider_cfg::ResolvedTarget::new(addr, bit))
                .collect(),
        )
    }

    #[test]
    fn apply_resolutions_a_wider_address_set_grows() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x2000, None), (0x3000, None)]),
            None,
        );
        assert!(progress.changed);
        assert!(
            progress.narrowed.is_empty(),
            "adding an address is discovery"
        );
        assert_eq!(progress.grew, vec![pcode_addr(0x1000)]);
        assert_eq!(folded, multiple(&[(0x2000, None), (0x3000, None)]));
    }

    #[test]
    fn apply_resolutions_a_lost_address_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
            multiple(&[(0x2000, None)]),
            None,
        );
        assert!(progress.changed);
        assert!(
            !progress.narrowed.is_empty(),
            "losing 0x3000 is not monotone discovery"
        );
        assert!(progress.grew.is_empty());
    }

    /// A caller seed is mode-less by construction, so the round that PROVES an
    /// interworking mode for a seeded target changes its key. Refining one
    /// successor is not losing one, and reading it as a loss would abandon a
    /// seeded trampoline chain that was merely refining.
    #[test]
    fn apply_resolutions_proving_a_mode_for_a_seeded_target_grows() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true))),
            Some(multiple(&[(0x2000, None)])),
        );
        assert!(progress.changed);
        assert!(
            progress.narrowed.is_empty(),
            "a None -> Some mode upgrade at a fixed address set is growth",
        );
        assert_eq!(
            folded,
            ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(0x2000, Some(true))),
        );
    }

    /// Two rounds proving OPPOSITE modes for one address decode it two
    /// different ways, so the induced edge set is not converging.
    #[test]
    fn apply_resolutions_a_mode_flip_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, Some(true))])),
            multiple(&[(0x2000, Some(false))]),
            None,
        );
        assert!(progress.changed);
        assert!(
            !progress.narrowed.is_empty(),
            "Thumb -> ARM at one address is a flip"
        );
        assert!(progress.grew.is_empty());
    }

    #[test]
    fn apply_resolutions_gaining_and_losing_in_one_round_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
            multiple(&[(0x2000, None), (0x4000, None)]),
            None,
        );
        assert!(progress.changed);
        assert!(!progress.narrowed.is_empty());
        assert!(progress.grew.is_empty());
    }

    /// Round 1 seats one interworking arm before the dispatch loop closes.
    /// Round 2 re-derives the now-seated `Switch`, which carries no ISA-mode
    /// input and so reports every arm mode-less, and the three NEW arms cannot
    /// be seated in a mode. The fold then equals the seated set, so the loop
    /// converges here: the site has to come back through `incomplete` or the
    /// caller reads a one-arm table alongside an empty `unresolved`.
    #[test]
    fn apply_resolutions_reports_a_site_whose_widening_was_dropped() {
        let (progress, folded) = round(
            Some(multiple(&[(0x1030, Some(true))])),
            multiple(&[
                (0x1030, None),
                (0x1040, None),
                (0x1050, None),
                (0x1060, None),
            ]),
            None,
        );
        assert!(
            !progress.changed,
            "the fold matches the seated set, so this round converges",
        );
        assert_eq!(
            concrete_targets(&folded),
            &[strider_cfg::ResolvedTarget::new(0x1030, Some(true))],
        );
        assert_eq!(
            progress.derived_incomplete,
            vec![pcode_addr(0x1000)],
            "a dropped widening must be reported, not silently seated",
        );
    }

    /// A fold that shrinks is applied, so the smaller set is what the next CFG
    /// is built from and the round after it reads as converged.
    #[test]
    fn apply_resolutions_reports_a_site_that_lost_an_address() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
            multiple(&[(0x2000, None)]),
            None,
        );
        assert!(!progress.narrowed.is_empty());
        assert_eq!(progress.derived_incomplete, vec![pcode_addr(0x1000)]);
    }

    /// The interworking test reads the flowing bit AT THE SITE, so two sites in
    /// one function can disagree about it: the cfg decodes a mode-less target in
    /// the mode at its own branch, not in the entry's.
    #[test]
    fn the_flowing_mode_is_read_per_site() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        // The site already proved Thumb at 0x2000; the round re-derives it
        // mode-less and adds 0x3000.
        let seated = multiple(&[(0x2000, Some(true))]);
        let rederived = multiple(&[(0x2000, None), (0x3000, None)]);

        let fold = |flowing: bool| {
            let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
            known.insert(addr, seated.clone());
            let mut resolutions: IndirectResolutions = FxHashMap::default();
            resolutions.insert(node, Some(rederived.clone()));
            let mut flowing_map: FxHashMap<PcodeInsnAddr, bool> = FxHashMap::default();
            flowing_map.insert(addr, flowing);
            apply_resolutions(
                &mut known,
                &FxHashMap::default(),
                &anchors,
                &Vec::new(),
                resolutions,
                &flowing_map,
                &rustc_hash::FxHashSet::default(),
            )
            .expect("apply");
            concrete_targets(&known[&addr]).len()
        };

        assert_eq!(
            fold(true),
            2,
            "Thumb flowing in: the proved mode matches, so the new arm is kept",
        );
        assert_eq!(
            fold(false),
            1,
            "ARM flowing in: the site interworks, so a mode-less new arm is a guess",
        );
    }

    /// A seed answers for the site; it says nothing about what the classifier
    /// derives there, so a derived loss is reported even at a seeded site.
    #[test]
    fn caller_seed_does_not_suppress_a_derived_loss() {
        let addr = pcode_addr(0x1000);
        let mut seed: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        seed.insert(addr, ResolvedTargets::Single(0x2000.into()));
        assert_eq!(
            live_unresolved_branches(
                &rustc_hash::FxHashSet::default(),
                &Vec::new(),
                &Vec::new(),
                &rustc_hash::FxHashSet::default(),
                &seed,
                &FxHashMap::default(),
                DerivedChannels {
                    budget_exhausted: &[],
                    incomplete_derived: &[addr],
                },
            ),
            vec![addr],
        );
    }

    #[test]
    fn apply_resolutions_equal_sized_oscillation_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x3000, None)]),
            None,
        );
        assert!(progress.changed);
        assert!(
            !progress.narrowed.is_empty(),
            "swapping one successor for another is an oscillation, not growth",
        );
    }
}
