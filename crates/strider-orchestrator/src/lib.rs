pub use strider_opt as opt;

pub use strider_cfg::{Incompleteness, Reason};
pub use strider_lift::lift::{LiftOutcome, Lifter};

mod resolve;

use std::borrow::Cow;
use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets, seated_in};
use strider_opt::{OptOptions, ReadOnlyMemory};

use resolve::{LoopState, Termination};

/// What `Default` lends: a `&'a` cannot borrow a temporary.
static NO_PER_ADDRESS_CCS: FxHashMap<u64, strider_target::BuiltCallingConvention> =
    FxHashMap::with_hasher(rustc_hash::FxBuildHasher);

/// [`Strider::analyze`]'s options. `Default` decodes an unbounded function
/// with no seeds and no CC overrides, optimises under
/// [`OptOptions::default`], compacts, and refuses an incomplete result.
#[derive(Clone, Debug)]
pub struct AnalyzeOptions<'a> {
    pub cfg: strider_cfg::CfgOptions<'a>,
    /// Keyed by direct-call target address, read by the lift only. Built
    /// against this engine's registers and fitting its arch, as the default CC
    /// must ([`Lifter::build_ir_with`]); borrowed, as
    /// [`strider_cfg::CfgOptions::known_targets`] is.
    pub per_address_ccs: &'a FxHashMap<u64, strider_target::BuiltCallingConvention>,
    pub opt: OptOptions,
    /// Drops unreachable nodes once the loop settles.
    pub compact: bool,
    /// Whether an incomplete result is returned or refused.
    pub on_incomplete: OnIncomplete,
}

impl Default for AnalyzeOptions<'_> {
    fn default() -> Self {
        Self {
            cfg: strider_cfg::CfgOptions::default(),
            per_address_ccs: &NO_PER_ADDRESS_CCS,
            opt: OptOptions::default(),
            compact: true,
            on_incomplete: OnIncomplete::default(),
        }
    }
}

/// What [`Strider::analyze`] and [`Strider::build_cfg`] do with a result that
/// is not complete.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnIncomplete {
    /// Refuse it: [`AnalyzeError::Incomplete`] names the sites.
    #[default]
    Error,
    /// Return it: its [`Incompleteness`] names the sites.
    Ignore,
}

impl OnIncomplete {
    /// `Err` naming `incomplete` when this policy refuses it.
    fn check(self, incomplete: &Incompleteness) -> std::result::Result<(), AnalyzeError> {
        if self == OnIncomplete::Error && !incomplete.is_complete() {
            return Err(AnalyzeError::Incomplete(incomplete.clone()));
        }
        Ok(())
    }
}

/// Why [`Strider::analyze`] or [`Strider::build_cfg`] returned no result.
#[derive(Debug)]
pub enum AnalyzeError {
    /// The analysis finished but cannot vouch for these sites, and
    /// [`OnIncomplete::Error`] refused it.
    Incomplete(Incompleteness),
    /// Decode, lift, optimisation or validation failed.
    Failed(anyhow::Error),
}

impl std::fmt::Display for AnalyzeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalyzeError::Incomplete(sites) => write!(f, "incomplete analysis: {sites}"),
            // The whole chain: this error offers no `source()` to walk.
            AnalyzeError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for AnalyzeError {}

impl From<anyhow::Error> for AnalyzeError {
    fn from(e: anyhow::Error) -> Self {
        AnalyzeError::Failed(e)
    }
}

/// Per-binary analysis handle.
pub struct Strider<R>
where
    R: rsleigh::MemReader,
{
    arch: strider_target::SleighArch,
    lifter: Lifter<R>,
    /// The read-only image `LoadReadOnly` folds against and the indirect-branch
    /// classifier reads jump tables from; `None` disables both.
    rom: Option<Box<dyn ReadOnlyMemory>>,
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// # Errors
    ///
    /// Returns `Err` from [`Lifter::new`]: `Sleigh::regs()` or
    /// `Sleigh::user_op_names()` failing, the arch declaring no address space
    /// with shortcut `'r'` or naming that space something other than `"ram"`,
    /// and `FlowVars::discover` failing.
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

    /// The engine `analyze` lifts with, the only one that can lift the CFG it
    /// returns.
    #[must_use]
    pub fn lifter(&self) -> &Lifter<R> {
        &self.lifter
    }

    /// The read-only image `LoadReadOnly` folds against and the
    /// indirect-branch classifier reads jump tables from, so a caller running
    /// its own pipeline builds the same `OptCtx` `analyze` does.
    #[must_use]
    pub fn rom(&self) -> Option<&dyn ReadOnlyMemory> {
        self.rom.as_deref()
    }

    /// Structural CFG build only: no lift, no optimisation, no
    /// indirect-branch resolution, so every dispatch the seeds do not seat is
    /// [`Reason::UnresolvedBranch`] and every seat [`Reason::UnverifiedSeed`].
    ///
    /// # Errors
    ///
    /// [`AnalyzeError::Incomplete`] when `on_incomplete` refuses
    /// [`strider_cfg::Cfg::incomplete`]; [`AnalyzeError::Failed`] for a
    /// build failure (see [`Lifter::build_cfg`]).
    pub fn build_cfg(
        &mut self,
        entry: u64,
        cfg_opts: &strider_cfg::CfgOptions,
        on_incomplete: OnIncomplete,
    ) -> std::result::Result<strider_cfg::Cfg, AnalyzeError> {
        let cfg = self
            .lifter
            .build_cfg(MachineInsnAddr::from(entry), cfg_opts)?;
        on_incomplete.check(cfg.incomplete())?;
        Ok(cfg)
    }

    /// Lift, optimise, and resolve indirect branches at `entry`.
    ///
    /// Resolution is a fixed-point re-lift loop: each round classifies indirect
    /// branches against the optimised IR, folds the targets into
    /// `known_targets` and re-lifts, until the induced edge set stops changing.
    /// `MAX_RESOLUTION_ITERATIONS` caps the folds (so at most one round more)
    /// and with them the discovery depth.
    /// Every narrowing is reported, and a site that narrows twice has an
    /// unstable answer: it is abandoned.
    ///
    /// What the loop cannot settle is reported: in
    /// [`AnalyzeError::Incomplete`] under the default [`OnIncomplete::Error`],
    /// or in [`AnalyzeResult::incomplete`] under [`OnIncomplete::Ignore`], an
    /// unresolved site with its placeholder still in the function. A site the
    /// CFG consumed as a `Return` or `TailCall` leaves no placeholder, so it is
    /// [`Reason::UnverifiedSeed`] (a `TailCall` whichever source derived it, a
    /// `Return` only where the CALLER seeded it), and also
    /// [`Reason::UnresolvedBranch`] if it grew before narrowing to
    /// `LinkRegister`.
    ///
    /// `opts.cfg.known_targets` is re-unioned into the working set every
    /// round and never mutated; a seed at an abandoned site is dropped from the
    /// working set. A seed suppresses the unresolved report of an unclassifiable
    /// seated `Switch` at its address only while the settled answer names
    /// nothing the seed did not; it never suppresses what the classifier
    /// derives. `opts.compact` applies once, after the loop.
    ///
    /// `pipeline` defaults to [`strider_opt::default_pipeline`];
    /// [`strider_opt::IndirectBranchClassify`] is appended unless it already
    /// runs.
    ///
    /// # Errors
    ///
    /// [`AnalyzeError::Incomplete`] when `opts.on_incomplete` refuses the
    /// result; [`AnalyzeError::Failed`] for a lift / cfg / opt / validation
    /// failure, never for an indirect branch.
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
    ) -> std::result::Result<AnalyzeResult, AnalyzeError> {
        // Built once and reused across re-lifts (`run` takes `&self`).
        let pipeline = &with_classify(pipeline.unwrap_or_else(strider_opt::default_pipeline));
        let start_addr = MachineInsnAddr::from(entry);
        let mut st = LoopState::new(opts.cfg.known_targets);
        // Every round rebuilds the whole function from the seat set: cfg, lift
        // and pipeline. Carrying a round's work forward would make the cfg a
        // function of more than the seats, which is what convergence rests on.
        let mut result = loop {
            let (round, resolutions) =
                self.build_round(start_addr, cc, opts, &st.working, pipeline)?;
            // A stale round still carries a seat the loop has since dropped,
            // so it is no answer even when nothing else moves.
            let stale = st.observe(&round);
            if round.is_settled() && !stale {
                break st.finish(round, Termination::Converged)?;
            }
            if st.folds == MAX_RESOLUTION_ITERATIONS {
                break st.finish(round, Termination::CapReached(resolutions))?;
            }
            if !st.fold(&round, resolutions)? && !stale {
                break st.finish(round, Termination::Converged)?;
            }
        };
        opts.on_incomplete.check(result.incomplete())?;
        if opts.compact {
            result.function.compact()?;
        }
        Ok(result)
    }
}

/// What [`Strider::analyze`] returns: the lifted function and the CFG it
/// was lifted from.
pub struct AnalyzeResult {
    /// The final round's CFG, the one `function` was lifted from, carrying
    /// this analysis's [`Incompleteness`] rather than that build's own.
    pub cfg: strider_cfg::Cfg,
    /// May still hold an `IndirectBranch` placeholder at a
    /// [`Reason::UnresolvedBranch`] site.
    pub function: strider_ir::Function,
}

impl AnalyzeResult {
    /// What this analysis cannot vouch for: every round's target reasons,
    /// since an inexact or stubbed edge fed the classifier whatever later
    /// rounds rebuild, and the final round's site reasons.
    #[must_use]
    pub fn incomplete(&self) -> &Incompleteness {
        self.cfg.incomplete()
    }

    /// Whether [`Self::incomplete`] is empty. `false` over
    /// [`Reason::UnverifiedSeed`] alone reads "complete but unverified",
    /// except for a caller-seeded `Return` whose re-check failed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.incomplete().is_complete()
    }
}

/// Caps the folds, so a run builds at most one more round than this.
///
/// Doubles as a DEPTH limit: one iteration seats one LEVEL of indirect-branch
/// discovery, so a chain of trampolines each jumping to the next needs one
/// round per link. [`apply_resolutions`] reporting no change is the
/// terminator for everything shallower. Exhausting the cap is never
/// [`AnalyzeError::Failed`]: the sites still growing are reported, and a site
/// whose answer never settles is abandoned and reported the same way.
const MAX_RESOLUTION_ITERATIONS: usize = 256;

/// `pipeline` running [`strider_opt::IndirectBranchClassify`], at most once: a
/// caller-supplied pipeline that already registers it would otherwise repeat
/// the pass's known-bits / dominator / value-range setup every re-lift round.
fn with_classify(mut pipeline: strider_opt::OptimizerPipeline) -> strider_opt::OptimizerPipeline {
    if !pipeline.has_post_pass::<strider_opt::IndirectBranchClassify>() {
        pipeline.add_post_pass(strider_opt::IndirectBranchClassify);
    }
    pipeline
}

/// One round's effect on the induced edge set.
#[derive(Default)]
struct Progress {
    /// Addresses whose successor set changed without losing ground, which
    /// includes a target taking a proved mode at an unchanged address set
    /// ([`narrows`]).
    grew: Vec<PcodeInsnAddr>,
    /// Addresses that LOST a successor, so the signal is not monotone there.
    narrowed: Vec<PcodeInsnAddr>,
    /// Addresses whose fold dropped a successor the CLASSIFIER proved this
    /// round, or whose answer stopped being a superset. A seed says nothing
    /// about what the classifier derives, so this list is never suppressed.
    /// A successor carried over from an earlier round needs no list of its
    /// own: dropping one is exactly what stops the answer being a superset.
    derived_incomplete: Vec<PcodeInsnAddr>,
    /// Addresses that seated a NEW arm on a mode nothing proved for that arm.
    /// The arm set is whole, so this is an unverified answer rather than an
    /// incomplete one.
    assumed_modes: Vec<PcodeInsnAddr>,
}

impl Progress {
    /// Some address's successor set differs from the previous round's: the
    /// loop's progress signal.
    fn changed(&self) -> bool {
        !self.grew.is_empty() || !self.narrowed.is_empty()
    }
}

/// Fold `resolutions` into `known_targets`, keyed back to pcode addresses
/// via `unresolved`.
///
/// Within one round, two distinct placeholders can share one `PcodeInsnAddr`;
/// their target sets are UNIONED, not last-write-wins. Across rounds the
/// staged set REPLACES the previous round's entry.
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
    // Across rounds, this round's staged set REPLACES the previous round's
    // entry (within the round they merged above), so a set that narrows from
    // unseatable to seatable once other branches resolve is not stranded;
    // convergence rides on the per-address diff, so an unchanged cone
    // re-inserts an equal value and reads as no growth. Every successor the
    // round reported yet did not seat comes back in
    // `Progress::derived_incomplete`.
    let mut progress = Progress::default();
    for (addr, targets) in staged {
        // `seated_in`, not a bare lookup: a caller can only spell the MACHINE
        // address, so at a site whose `BRANCHIND` is not the instruction's
        // first p-code op the answer to adopt modes from is keyed there.
        let known = seated_in(known_targets, addr);
        // A re-derivation reports no mode where the dispatch commits none,
        // including at an address a caller seed gave a mode. Re-deriving must
        // widen the arm set, not re-decode the old arms in the mode flowing
        // into the branch.
        let (targets, assumed_mode) = adopt_known_modes(known, targets, flowing_at(addr));
        // The caller's own seed is unioned in by ADDRESS every round, after
        // both mode filters and never subject to them: a seed carries a mode
        // only if the caller built one, and filtering it against a mode-bearing
        // classification would delete the whole caller answer.
        let targets = match seated_in(caller_seed, addr) {
            Some(seed) => combine_resolved(seed, &targets, None),
            None => targets,
        };
        let seated: rustc_hash::FxHashSet<Option<u64>> =
            targets_of(&targets).map(|(target, _bit)| target).collect();
        let derived_dropped = reported
            .get(&addr)
            .into_iter()
            .flatten()
            .any(|a| !seated.contains(a));
        // `seated_in` again, for the reason the seat used it: the round that
        // produced this IR seated through `CfgOptions::seated`, so at a site
        // whose `BRANCHIND` is not the instruction's first p-code op an exact
        // lookup reads empty and every unchanged round diffs as growth.
        let prev = seated_in(known_targets, addr)
            .map(target_keys)
            .unwrap_or_default();
        let next = target_keys(&targets);
        let narrowed = narrows(&prev, &next);
        if prev != next {
            if narrowed {
                progress.narrowed.push(addr);
            } else {
                progress.grew.push(addr);
            }
        }
        if derived_dropped || narrowed {
            progress.derived_incomplete.push(addr);
        }
        // Decodable, and the arm set is whole: nothing evaluated THIS arm's
        // mode, which is `UnverifiedSeed`, not `UnresolvedBranch`.
        if assumed_mode {
            progress.assumed_modes.push(addr);
        }
        known_targets.insert(addr, targets);
    }
    Ok(progress)
}

/// The ISA-mode bit a target seated with no mode of its own decodes in, per
/// indirect site.
///
/// Read off the cfg, which sampled it while each site's region was decoding
/// (`Builder::sample_flowing_isa_bit`). Probing the live engine again here
/// would answer for whichever region painted the branch address last, and
/// [`adopt_known_modes`] would then take the wrong `interworking` verdict.
/// [`strider_cfg::Cfg::function_isa_bit`] covers an address no indirect branch
/// was sealed at and every arch with no ISA-mode var.
fn flowing_isa_bits(
    cfg: &strider_cfg::Cfg,
    unresolved: &UnresolvedAnchors,
    switch_anchors: &UnresolvedAnchors,
) -> FxHashMap<PcodeInsnAddr, bool> {
    let entry_bit = cfg.function_isa_bit().unwrap_or(false);
    unresolved
        .iter()
        .chain(switch_anchors.iter())
        .map(|(addr, _)| {
            (
                *addr,
                cfg.flowing_isa_bit_at_site(*addr).unwrap_or(entry_bit),
            )
        })
        .collect()
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
    let Some(seed) = seated_in(caller_seed, addr) else {
        return false;
    };
    let seeded: rustc_hash::FxHashSet<u64> =
        visit(concrete_targets(seed)).map(|t| t.addr).collect();
    settled.get(&addr).is_none_or(|final_targets| {
        visit(concrete_targets(final_targets)).all(|t| seeded.contains(&t.addr))
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
/// identically, so the set still widens. Only the arms already evaluated
/// proved that mode, and they are no evidence about an arm nobody has
/// evaluated, so a later round widening the index can seat an address whose
/// committed mode was never checked. Dropping it would cost every same-mode
/// dispatch its arms, so it is kept and the second return value reports the
/// site through [`Progress::assumed_modes`]: the arm set is whole, which is an
/// UNVERIFIED answer rather than an incomplete one.
fn adopt_known_modes(
    known: Option<&ResolvedTargets>,
    targets: ResolvedTargets,
    flowing_isa_bit: bool,
) -> (ResolvedTargets, bool) {
    let Some(known) = known else {
        return (targets, false);
    };
    let known_targets = concrete_targets(known);
    let modes: FxHashMap<u64, bool> = visit(known_targets)
        .filter_map(|t| t.isa_bit.map(|bit| (t.addr, bit)))
        .collect();
    if modes.is_empty() || matches!(targets, ResolvedTargets::LinkRegister) {
        return (targets, false);
    }
    let interworking = modes.values().any(|&bit| bit != flowing_isa_bit);
    let known_addrs: rustc_hash::FxHashSet<u64> = visit(known_targets).map(|t| t.addr).collect();
    let mut assumed = false;
    let from_single = matches!(targets, ResolvedTargets::Single(_));
    let kept: Vec<strider_cfg::ResolvedTarget> = visit(concrete_targets(&targets))
        .filter_map(|t| match (t.isa_bit, modes.get(&t.addr)) {
            (Some(_), _) => Some(*t),
            (None, Some(&bit)) => Some(strider_cfg::ResolvedTarget::new(t.addr, Some(bit))),
            (None, None) if known_addrs.contains(&t.addr) => Some(*t),
            // New to a mode-committing site, and the site does not interwork so
            // the flowing mode decodes it the same way the proved arms decode.
            // Kept, and flagged: nobody evaluated THIS arm's mode.
            (None, None) => (!interworking).then(|| {
                assumed = true;
                *t
            }),
        })
        .collect();
    (resolved_from(kept, from_single), assumed)
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
        .chain(visit(tail).map(|t| (Some(t.addr), t.isa_bit)))
}

/// `side`, counting each target read in tests: the unit the fold's cost
/// bounds are stated in.
fn visit(
    side: &[strider_cfg::ResolvedTarget],
) -> impl Iterator<Item = &strider_cfg::ResolvedTarget> + '_ {
    side.iter().inspect(|_| {
        #[cfg(test)]
        tests::TARGET_VISITS.with(|v| v.set(v.get() + 1));
    })
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
/// IR, dropping a mode-less address only the non-interworking side knows: an
/// address reported with no mode decodes in `flowing_isa_bit`, which is a guess
/// only against a side proving a DIFFERENT mode.
fn merge_resolved(
    a: &ResolvedTargets,
    b: &ResolvedTargets,
    flowing_isa_bit: bool,
) -> ResolvedTargets {
    combine_resolved(a, b, Some(flowing_isa_bit))
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
    let single_addr = |r: &ResolvedTargets| match r {
        ResolvedTargets::Single(t) => Some(t.addr),
        _ => None,
    };
    let single_addrs = [single_addr(a), single_addr(b)];
    let a_targets = concrete_targets(a);
    let b_targets = concrete_targets(b);
    let interworks = |side: &[strider_cfg::ResolvedTarget]| {
        flowing_isa_bit
            .is_some_and(|flowing| visit(side).any(|t| t.isa_bit.is_some_and(|bit| bit != flowing)))
    };
    let a_interworks = interworks(a_targets);
    let b_interworks = interworks(b_targets);
    let addrs = |side: &[strider_cfg::ResolvedTarget]| -> rustc_hash::FxHashSet<u64> {
        visit(side).map(|t| t.addr).collect()
    };
    let a_addrs = addrs(a_targets);
    let b_addrs = addrs(b_targets);
    let guessed = |t: &strider_cfg::ResolvedTarget,
                   own_interworks: bool,
                   other_interworks: bool,
                   other_addrs: &rustc_hash::FxHashSet<u64>| {
        other_interworks && !own_interworks && t.isa_bit.is_none() && !other_addrs.contains(&t.addr)
    };
    let mut targets: Vec<strider_cfg::ResolvedTarget> = visit(a_targets)
        .filter(|t| !guessed(t, a_interworks, b_interworks, &b_addrs))
        .chain(visit(b_targets).filter(|t| !guessed(t, b_interworks, a_interworks, &a_addrs)))
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
    let from_single = matches!(targets.as_slice(), [t] if single_addrs.contains(&Some(t.addr)));
    resolved_from(targets, from_single)
}

/// `targets` as a [`ResolvedTargets`], and the ONE place an EMPTY one is built.
///
/// `strider_cfg` documents `Multiple` as non-empty and defends: it seats no
/// `Switch` for an empty set, re-emitting the placeholder, so the site comes
/// back as a live unresolved branch. That is the honest answer once every
/// target has been contradicted (the enum spells no "nothing trustworthy
/// here") where staging nothing would leave the previous round's answer
/// seated as if it had never been contradicted.
///
/// `Single` only when `from_single`, i.e. the survivor is a `Single` input's
/// target: the cfg seats an out-of-range `Single` as a tail call but defers an
/// out-of-range arm, so a merge must not change which.
fn resolved_from(targets: Vec<strider_cfg::ResolvedTarget>, from_single: bool) -> ResolvedTargets {
    match targets.as_slice() {
        [single] if from_single => ResolvedTargets::Single(*single),
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

/// Stops the loop resolving `site`, dropping its seat.
///
/// Both keys go, and both are marked: a caller can only spell the MACHINE
/// address ([`seated_in`]), so a site whose `BRANCHIND` is not the
/// instruction's first p-code op holds two entries, and
/// [`strider_cfg::CfgOptions::seated`] falls back to the machine-start one.
/// Removing the anchor key alone re-seats the caller's arms at a site the loop
/// has given up on.
fn abandon_site(
    known_targets: &mut FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    abandoned: &mut rustc_hash::FxHashSet<PcodeInsnAddr>,
    site: PcodeInsnAddr,
) {
    for key in [
        site,
        PcodeInsnAddr::at_machine_start(site.machine_addr.addr),
    ] {
        known_targets.remove(&key);
        abandoned.insert(key);
    }
}

/// Every `BranchIndirect` anchor a cfg carries, seated or deferred, indexed
/// for [`DispatchAnchors::anchor_of`]: built once per round, looked up once per
/// `known_targets` entry.
struct DispatchAnchors {
    anchors: rustc_hash::FxHashSet<PcodeInsnAddr>,
    /// Lowest anchor of each machine instruction.
    first_at: FxHashMap<u64, PcodeInsnAddr>,
}

impl DispatchAnchors {
    fn new(cfg: &strider_cfg::Cfg) -> Self {
        Self::from_addrs(cfg.regions().filter_map(|region| match &region.terminator {
            strider_cfg::RegionTerminator::Switch { addr, .. }
            | strider_cfg::RegionTerminator::UnresolvedIndirectBranch { addr, .. } => Some(*addr),
            _ => None,
        }))
    }

    fn from_addrs(addrs: impl IntoIterator<Item = PcodeInsnAddr>) -> Self {
        let mut this = Self {
            anchors: rustc_hash::FxHashSet::default(),
            first_at: FxHashMap::default(),
        };
        for addr in addrs {
            this.anchors.insert(addr);
            this.first_at
                .entry(addr.machine_addr.addr)
                .and_modify(|first| *first = (*first).min(addr))
                .or_insert(addr);
        }
        this
    }

    /// The `BranchIndirect` address a `known_targets` key stands for.
    ///
    /// A caller can only spell the machine address ([`seated_in`]), so its key
    /// is the instruction's, not the dispatch's. Falls back to the key itself
    /// for a seat the cfg CONSUMED, which leaves no anchor to name.
    fn anchor_of(&self, site: PcodeInsnAddr) -> PcodeInsnAddr {
        if self.anchors.contains(&site) {
            return site;
        }
        self.first_at
            .get(&site.machine_addr.addr)
            .copied()
            .unwrap_or(site)
    }
}

/// Abandons every site naming a target the round's cfg could not trust,
/// returning those sites so the caller can report them.
///
/// Two ways a target goes bad, and they carry different verdicts.
///
/// A classifier that over-approximates a jump-table bound reaches past the
/// table and names addresses that are not code. The bound is what is wrong, so
/// no arm of that answer is trustworthy and the whole seat of the SITE that
/// named it goes. The same verdict covers an arm whose own target decodes but
/// which reaches code that does not, through a direct branch out of it: the
/// cfg reports the arm, not the address that failed, so the failing address
/// is reported under no reason. The same address seated from another site decodes
/// in that site's own committed context, so its failure there is no verdict
/// here.
///
/// An address two edges reach in different ISA modes is decoded once, in
/// whichever mode won the work queue, so the other edge's arm is not the
/// instruction stream it believes. ANY two edges raise that, direct ones
/// included (`Builder::explore` and `RegionBuilder::note_isa_mode_clash`), so
/// it is no verdict on the rest of the table: only the arm naming the clashing
/// address goes, and the site keeps the arms it still seats.
///
/// Either way the site is frozen, taking no further classification: the arms
/// dropped here are exactly the ones the classifier re-derives every round, so
/// a site left open would re-seat them and cycle until the iteration cap.
fn abandon_undecodable(
    undecodable_targets: &[strider_cfg::UndecodableTarget],
    clashing_targets: &[PcodeInsnAddr],
    dispatch_anchors: &DispatchAnchors,
    abandoned: &mut rustc_hash::FxHashSet<PcodeInsnAddr>,
    known_targets: &mut Cow<'_, FxHashMap<PcodeInsnAddr, ResolvedTargets>>,
) -> Vec<PcodeInsnAddr> {
    let addrs = |sites: &[PcodeInsnAddr]| -> rustc_hash::FxHashSet<u64> {
        sites.iter().map(|a| a.machine_addr.addr).collect()
    };
    // Both keys of each site, since a caller can only spell the machine address
    // ([`seated_in`]) while the cfg reports the `BRANCHIND`'s own p-code address.
    // The anchor rides along: it is the address every other reason names the
    // site by, and the machine-start key is not it.
    let mut undecodable: FxHashMap<PcodeInsnAddr, (PcodeInsnAddr, rustc_hash::FxHashSet<u64>)> =
        FxHashMap::default();
    for bad in undecodable_targets {
        for key in [
            bad.site,
            PcodeInsnAddr::at_machine_start(bad.site.machine_addr.addr),
        ] {
            undecodable
                .entry(key)
                .or_insert_with(|| (bad.site, rustc_hash::FxHashSet::default()))
                .1
                .insert(bad.target.machine_addr.addr);
        }
    }
    let clashing = addrs(clashing_targets);
    if undecodable.is_empty() && clashing.is_empty() {
        return Vec::new();
    }
    let names = |targets: &ResolvedTargets, bad: &rustc_hash::FxHashSet<u64>| {
        concrete_targets(targets)
            .iter()
            .any(|t| bad.contains(&t.addr))
    };
    // Keyed on the entry it iterated, not on `abandon_site`'s two keys: one
    // instruction can hold both a caller seed and a fold, and rewriting either
    // through the other's key makes the surviving arms depend on map order.
    let hit: Vec<(
        PcodeInsnAddr,
        PcodeInsnAddr,
        Vec<strider_cfg::ResolvedTarget>,
    )> = known_targets
        .iter()
        .filter_map(|(site, targets)| {
            if let Some((anchor, bad)) = undecodable.get(site)
                && names(targets, bad)
            {
                return Some((*site, *anchor, Vec::new()));
            }
            names(targets, &clashing).then(|| {
                let kept = concrete_targets(targets)
                    .iter()
                    .filter(|t| !clashing.contains(&t.addr))
                    .copied()
                    .collect();
                (*site, dispatch_anchors.anchor_of(*site), kept)
            })
        })
        .collect();
    let mut out = Vec::with_capacity(hit.len());
    for (site, anchor, kept) in hit {
        // Every key of the site, the way `abandon_site` marks them:
        // `apply_resolutions` tests this set with the anchor's own p-code
        // address, which is not the key a machine-start seed was found under.
        for key in [
            site,
            anchor,
            PcodeInsnAddr::at_machine_start(site.machine_addr.addr),
        ] {
            abandoned.insert(key);
        }
        if kept.is_empty() {
            known_targets.to_mut().remove(&site);
        } else {
            // Only a `Multiple` keeps arms here: a `Single` named a clashing
            // target, its only one.
            known_targets
                .to_mut()
                .insert(site, resolved_from(kept, false));
        }
        // The anchor, not the entry's key: an address no anchor in this run
        // uses would be reported unresolved naming nothing.
        out.push(anchor);
    }
    out
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
    // above cannot see it and no other reason names it either. The seat is
    // the only record that a caller's answer replaced whatever the classifier
    // would have derived. Only a CALLER's here: a seat the classifier derived
    // was its own answer, not an unchecked assertion, and `settle_return_sites`
    // re-checks every seat against the round's own IR either way.
    out.extend(
        cfg.link_register_seated()
            .iter()
            .filter(|addr| seated_in(caller_seed, **addr).is_some()),
    );
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
        // The lookup the CFG seats with, not an exact-key one.
        let Some(asked) = seated_in(known_targets, *addr) else {
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
///   arms are whatever the round that seated them proved, which may be a
///   proper subset, and seating consumed the placeholder, so this list is its
///   only report.
/// - `budget_exhausted`, the sites still growing when the iteration cap ran out.
/// - `incomplete_derived`, the sites whose fold dropped a successor the
///   classifier proved, whose answer stopped being a superset, or that the loop
///   abandoned.
///
/// A caller seed asserts the site is complete, which suppresses the second of
/// those, the unclassified seated `Switch`, but only while `settled`, the
/// site's final successor set, holds nothing the seed did not name. A site that
/// outgrew its seed disproves the assertion, and the arms past the seed came
/// from a classifier that has since gone silent, so nothing vouches for them.
/// [`unverified_seeded_sites`] applies the same test the other way round, so
/// the `switch_anchors` half of the two reports is complementary. The derived
/// channels below are appended unconditionally, so a site can still reach
/// both.
///
/// The seed suppresses nothing else. A live placeholder means the cfg could
/// not seat the seeded set (empty, or a target out of range or interior to an
/// instruction), so the seed did not settle the site. Outgrowing the seed is
/// itself evidence it was incomplete, which leaves `budget_exhausted` standing.
/// And `incomplete_derived` reports what the classifier proved, which a seed is
/// no licence to drop.
fn live_unresolved_branches(
    live_indirect: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    unresolved: &UnresolvedAnchors,
    switch_anchors: &UnresolvedAnchors,
    unclassified: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    caller_seed: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    settled: &FxHashMap<PcodeInsnAddr, ResolvedTargets>,
    derived: DerivedChannels<'_>,
) -> Vec<PcodeInsnAddr> {
    // `live_indirect` holds every reachable placeholder AND seated `Switch` of
    // the round that produced `function`. Intersecting it with `unresolved`,
    // which anchors only placeholders, selects the live ones.
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

/// The return sites whose target is not the function's entry return address.
/// Each target is taken back off the `Return` [`Strider::build_round`] anchored
/// it on.
///
/// Verification reads the assumption set the caller gave: a claim that holds
/// keeps the verdict right, and a claim that does not is the caller's. There
/// is no second, claim-cleared run to measure what a claim bought, because
/// nothing downstream would act on the difference.
///
/// A site the pipeline culled took its `Return` with it.
fn settle_return_sites(
    function: &mut strider_ir::Function,
    sites: &[strider_lift::lift::ReturnSite],
    assumptions: &strider_opt::AssumptionOptions,
) -> Vec<PcodeInsnAddr> {
    let mut unverified = Vec::new();
    let mut live = Vec::with_capacity(sites.len());
    let mut edit = strider_opt::EditFunction::new(function);
    {
        let mut targets = strider_opt::ReturnTargets::new(edit.function(), assumptions);
        for &(addr, ret, _) in sites {
            if !edit.is_live(ret) {
                continue;
            }
            let inputs = strider_ir::IRViewer::node_inputs(edit.function(), ret);
            let anchored = inputs.len() - 1;
            let &target = inputs
                .get(anchored)
                .expect("an anchored Return carries its target last");
            if !targets.returns_to_caller(ret, target) {
                unverified.push(addr);
            }
            live.push((ret, anchored));
        }
    }
    for (ret, anchored) in live {
        edit.truncate_node_inputs(ret, anchored);
    }
    edit.clean();
    unverified
}

/// Every reachable `IndirectBranch` placeholder and seated `Switch`.
fn live_dispatch_sites(
    function: &strider_ir::Function,
) -> rustc_hash::FxHashSet<strider_ir::node::NodeId> {
    use strider_ir::IRWalker;
    use strider_ir::node::NodeKind;
    function
        .walk_kind(|k| matches!(k, NodeKind::IndirectBranch | NodeKind::Switch(_)))
        .collect()
}

/// The live sites this round derived no targets for, including any the
/// classification map does not name.
fn unclassified_nodes(
    live_sites: &rustc_hash::FxHashSet<strider_ir::node::NodeId>,
    resolutions: &IndirectResolutions,
) -> rustc_hash::FxHashSet<strider_ir::node::NodeId> {
    live_sites
        .iter()
        .filter(|node| !matches!(resolutions.get(node), Some(Some(_))))
        .copied()
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

    thread_local! {
        /// Targets [`visit`] read on this thread.
        pub(super) static TARGET_VISITS: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
    }

    /// `run(n)`'s target visits at two sizes 8x apart: linear is 8x the
    /// visits, quadratic 64x.
    fn assert_linear_visits(what: &str, run: impl Fn(u64)) {
        let visits = |n| {
            TARGET_VISITS.with(|v| v.set(0));
            run(n);
            TARGET_VISITS.with(std::cell::Cell::get)
        };
        let small = visits(1_000);
        let large = visits(8_000);
        assert!(
            small >= 1_000,
            "{what}: {small} visits cannot read every arm"
        );
        assert!(
            large < small * 9,
            "{what}: 8x the arms cost {large} visits against {small}",
        );
    }

    fn pcode_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(machine),
            insn_index: 0,
        }
    }

    /// The cause chain, as `{:#}` prints it: callers format this error, and it
    /// offers no `source()` to walk.
    #[test]
    fn a_failure_displays_its_whole_chain() {
        let e = AnalyzeError::from(anyhow!("inner").context("outer"));
        assert_eq!(e.to_string(), "outer: inner");
        assert!(std::error::Error::source(&e).is_none());
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

    /// The report key is the LIVE placeholder, not the site's classification
    /// state: the cfg layer re-emits a placeholder for a target set it could
    /// not seat, and that site must still be named.
    #[test]
    fn live_unresolved_reports_live_unclassified_branch() {
        let (function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let live = live_dispatch_sites(&function);
        assert_eq!(
            live_placeholders(&live, &unresolved),
            vec![addr],
            "a live placeholder must be reported whatever its site's \
             classification state",
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
        let live = live_dispatch_sites(&function);
        assert!(
            live_placeholders(&live, &unresolved).is_empty(),
            "a dead / non-live IndirectBranch placeholder must not be reported"
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
            !progress.changed(),
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
            progress.changed(),
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
            progress.changed(),
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
        assert_eq!(
            resolved_from(Vec::new(), true),
            ResolvedTargets::Multiple(vec![])
        );
        assert_eq!(
            resolved_from(vec![0x2000.into()], true),
            ResolvedTargets::Single(0x2000.into()),
        );
        assert_eq!(
            resolved_from(vec![0x2000.into()], false),
            ResolvedTargets::Multiple(vec![0x2000.into()]),
        );
    }

    /// Two anchors at ONE pcode address in one round, one of them a `Switch`
    /// re-derivation with no mode to evaluate, which reports `None` for every
    /// address it discovers. The cfg reads that as "inherit the branch's
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
        assert_linear_visits("merging contradicted arms", |arms| {
            let thumb = ResolvedTargets::Multiple(
                (0..arms)
                    .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true)))
                    .collect(),
            );
            let arm = ResolvedTargets::Multiple(
                (0..arms)
                    .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(false)))
                    .collect(),
            );
            let merged = merge_resolved(&thumb, &arm, false);
            assert!(concrete_targets(&merged).is_empty());
        });
    }

    /// Scales ARMS at one site, where
    /// [`apply_resolutions_does_not_go_quadratic_in_accumulated_targets`]
    /// scales addresses: a mode-less re-derivation of an interworking table
    /// checks every new arm against the seated set.
    #[test]
    fn apply_resolutions_does_not_go_quadratic_in_arms_at_one_site() {
        use strider_cfg::ResolvedTarget;
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        assert_linear_visits("folding seated arms against re-derived ones", |arms| {
            let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
            known.insert(
                addr,
                ResolvedTargets::Multiple(
                    (0..arms)
                        .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, Some(true)))
                        .collect(),
                ),
            );
            let mut resolutions: IndirectResolutions = FxHashMap::default();
            resolutions.insert(
                node,
                Some(ResolvedTargets::Multiple(
                    (0..arms * 2)
                        .map(|i| ResolvedTarget::new(0x2_0000 + i * 4, None))
                        .collect(),
                )),
            );
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
            assert_eq!(progress.derived_incomplete, vec![addr]);
        });
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
    /// accumulated for every other address.
    #[test]
    fn apply_resolutions_does_not_go_quadratic_in_accumulated_targets() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let unresolved: UnresolvedAnchors = vec![(addr, node)];
        let visits = |accumulated: u64| {
            let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = (0..accumulated)
                .map(|i| {
                    (
                        pcode_addr(0x10_0000 + i * 4),
                        ResolvedTargets::Multiple(vec![
                            (0x20_0000 + i * 8).into(),
                            (0x20_0004 + i * 8).into(),
                        ]),
                    )
                })
                .collect();
            let mut resolutions: IndirectResolutions = FxHashMap::default();
            resolutions.insert(
                node,
                Some(ResolvedTargets::Multiple(vec![
                    0x2000.into(),
                    0x2004.into(),
                ])),
            );
            TARGET_VISITS.with(|v| v.set(0));
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
            assert!(progress.changed());
            TARGET_VISITS.with(std::cell::Cell::get)
        };
        let alone = visits(1);
        assert!(alone > 0, "the merge reads its own targets");
        assert_eq!(
            visits(200_000),
            alone,
            "merging one resolution reads targets of other addresses",
        );
    }

    /// Round 1 seats an interworking table with a proven per-target mode.
    /// Round 2 re-derives the now-seated `Switch` with no mode to evaluate, so
    /// every target comes back mode-less. The seated modes must
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
        assert!(progress.changed());
        assert_eq!(folded, multiple(&[(0x2000, None), (0x2004, None)]));
    }

    /// A proved mode EQUAL to the flowing one seats identically to no mode at
    /// all, so the set still widens: the drop is for sites that switch ISA.
    ///
    /// The site is still reported: 0x2004's own mode was never evaluated, and
    /// only the OTHER arms proved the one it is decoded in, so seating it stays
    /// an assumption. Dropping the arm instead would cost every same-mode
    /// dispatch its widening; reporting keeps the arm and refuses to call the
    /// answer settled.
    #[test]
    fn apply_resolutions_widens_a_site_whose_proved_mode_is_the_flowing_one() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, Some(false))])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert!(progress.changed());
        assert_eq!(folded, multiple(&[(0x2000, Some(false)), (0x2004, None)]));
        assert!(
            progress.derived_incomplete.is_empty(),
            "nothing was dropped and nothing narrowed"
        );
        assert_eq!(
            progress.assumed_modes,
            vec![pcode_addr(0x1000)],
            "an arm seated on a mode nothing proved for it must be reported"
        );
    }

    /// The same site, re-derived with nothing new, assumes nothing and so
    /// reports nothing: the flag is for arms the site had not already seated.
    #[test]
    fn apply_resolutions_reports_nothing_when_no_new_arm_is_assumed() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, Some(false)), (0x2004, Some(false))])),
            multiple(&[(0x2000, None), (0x2004, None)]),
            None,
        );
        assert_eq!(
            folded,
            multiple(&[(0x2000, Some(false)), (0x2004, Some(false))])
        );
        assert!(progress.assumed_modes.is_empty());
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

    /// A one-arm table answer keeps its shape through a merge: the cfg seats an
    /// out-of-range `Single` as a tail call but defers an out-of-range arm, so
    /// whether a seed or a known mode passed through must not decide which.
    #[test]
    fn apply_resolutions_keeps_a_one_arm_multiple_a_multiple() {
        let (_progress, seeded) = round(
            None,
            multiple(&[(0x2000, None)]),
            Some(multiple(&[(0x2000, None)])),
        );
        assert_eq!(
            seeded,
            multiple(&[(0x2000, None)]),
            "through the caller-seed union"
        );
        let (_progress, moded) = round(
            Some(multiple(&[(0x2000, Some(true))])),
            multiple(&[(0x2000, None)]),
            None,
        );
        assert_eq!(
            moded,
            multiple(&[(0x2000, Some(true))]),
            "through a known mode"
        );
    }

    /// A merge answers `Single` only when the surviving target is the `Single`
    /// input's own: an arm left over after the `Single`'s target is dropped
    /// stays an arm.
    #[test]
    fn combine_resolved_keeps_a_surviving_table_arm_an_arm() {
        let single =
            |addr, bit| ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(addr, bit));
        assert_eq!(
            combine_resolved(
                &single(0x2000, Some(true)),
                &multiple(&[(0x2000, Some(false)), (0x3000, None)]),
                None,
            ),
            multiple(&[(0x3000, None)]),
            "the Single's target dropped as contradicted",
        );
        assert_eq!(
            combine_resolved(
                &single(0x2000, None),
                &multiple(&[(0x3000, Some(true))]),
                Some(false),
            ),
            multiple(&[(0x3000, Some(true))]),
            "the Single's mode-less target dropped as guessed",
        );
        assert_eq!(
            combine_resolved(&single(0x2000, None), &multiple(&[(0x2000, None)]), None),
            single(0x2000, None),
            "the Single's own target survives",
        );
        assert_eq!(
            combine_resolved(
                &multiple(&[(0x2000, None)]),
                &multiple(&[(0x2000, None)]),
                None,
            ),
            multiple(&[(0x2000, None)]),
            "two one-arm tables",
        );
    }

    #[test]
    fn apply_resolutions_a_wider_address_set_grows() {
        let (progress, folded) = round(
            Some(multiple(&[(0x2000, None)])),
            multiple(&[(0x2000, None), (0x3000, None)]),
            None,
        );
        assert!(progress.changed());
        assert!(
            progress.narrowed.is_empty(),
            "adding an address is discovery"
        );
        assert_eq!(progress.grew, vec![pcode_addr(0x1000)]);
        assert_eq!(folded, multiple(&[(0x2000, None), (0x3000, None)]));
    }

    /// The shrunken fold is applied, so the smaller set is what the next CFG is
    /// built from and the round after it reads as converged.
    #[test]
    fn apply_resolutions_a_lost_address_narrows() {
        let (progress, _folded) = round(
            Some(multiple(&[(0x2000, None), (0x3000, None)])),
            multiple(&[(0x2000, None)]),
            None,
        );
        assert!(progress.changed());
        assert!(
            !progress.narrowed.is_empty(),
            "losing 0x3000 is not monotone discovery"
        );
        assert!(progress.grew.is_empty());
        assert_eq!(progress.derived_incomplete, vec![pcode_addr(0x1000)]);
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
        assert!(progress.changed());
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
        assert!(progress.changed());
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
        assert!(progress.changed());
        assert!(!progress.narrowed.is_empty());
        assert!(progress.grew.is_empty());
    }

    /// Round 1 seats one interworking arm before the dispatch loop closes.
    /// Round 2 re-derives the now-seated `Switch` with no mode to evaluate, so
    /// it reports every arm mode-less, and the three NEW arms cannot be seated
    /// in a mode. The fold then equals the seated set, so the loop
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
            !progress.changed(),
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
        assert!(progress.changed());
        assert!(
            !progress.narrowed.is_empty(),
            "swapping one successor for another is an oscillation, not growth",
        );
    }

    /// A seeded arm at `target` that would not decode, seated from `site`.
    fn undecodable(site: PcodeInsnAddr, target: u64) -> strider_cfg::UndecodableTarget {
        strider_cfg::UndecodableTarget {
            site,
            target: pcode_addr(target),
        }
    }

    /// The `BRANCHIND` of an ARM `bx`, a MIPS `jr` or an x86 `jmp [mem]` is not
    /// the instruction's first p-code op, so the anchor key and the only key a
    /// caller can spell are two entries for one site.
    fn mid_insn_addr(machine: u64) -> PcodeInsnAddr {
        PcodeInsnAddr {
            machine_addr: MachineInsnAddr::from(machine),
            insn_index: 2,
        }
    }

    #[test]
    fn abandoning_a_site_removes_the_caller_key_too() {
        let anchor = mid_insn_addr(0x1000);
        let start = pcode_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(anchor, multiple(&[(0x2000, None)]));
        known.insert(start, multiple(&[(0x3000, None)]));
        let mut abandoned = rustc_hash::FxHashSet::default();

        abandon_site(&mut known, &mut abandoned, anchor);

        assert!(
            known.is_empty(),
            "`CfgOptions::seated` falls back to the machine-start key, so a \
             caller seed left there re-seats a site the loop gave up on",
        );
        assert!(abandoned.contains(&anchor) && abandoned.contains(&start));
    }

    #[test]
    fn an_undecodable_target_abandons_the_whole_site() {
        let site = mid_insn_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(site, multiple(&[(0x2000, None), (0x9000, None)]));
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[undecodable(site, 0x9000)],
            &[],
            &DispatchAnchors::from_addrs([]),
            &mut abandoned,
            &mut known,
        );

        assert_eq!(hit, vec![site]);
        assert!(
            !known.contains_key(&site),
            "an over-approximated table bound is no verdict on one arm, so no \
             arm of that answer survives",
        );
        assert!(abandoned.contains(&site));
    }

    /// One site's undecodable arm is no verdict on another site naming the same
    /// address: each decodes in its own committed context.
    #[test]
    fn an_undecodable_target_freezes_only_the_site_that_named_it() {
        let bad = mid_insn_addr(0x1000);
        let other = pcode_addr(0x1100);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(bad, multiple(&[(0x2000, None), (0x9000, None)]));
        known.insert(other, multiple(&[(0x9000, None)]));
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[undecodable(bad, 0x9000)],
            &[],
            &DispatchAnchors::from_addrs([]),
            &mut abandoned,
            &mut known,
        );

        assert_eq!(hit, vec![bad]);
        assert!(!known.contains_key(&bad));
        assert_eq!(
            target_keys(&known[&other]),
            target_keys(&multiple(&[(0x9000, None)])),
        );
        assert!(!abandoned.contains(&other));
    }

    /// An ISA clash is raised by ANY two edges reaching one address, direct
    /// ones included, so it says nothing about the table's other arms.
    #[test]
    fn an_isa_clash_drops_only_the_clashing_arm() {
        let site = mid_insn_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(
            site,
            multiple(&[(0x2000, None), (0x3000, None), (0x4000, None)]),
        );
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[],
            &[pcode_addr(0x3000)],
            &DispatchAnchors::from_addrs([]),
            &mut abandoned,
            &mut known,
        );

        assert_eq!(hit, vec![site]);
        assert_eq!(
            target_keys(&known[&site]),
            target_keys(&multiple(&[(0x2000, None), (0x4000, None)])),
        );
        assert!(
            abandoned.contains(&site),
            "the classifier re-derives the dropped arm every round, so a site \
             left open re-seats it until the iteration cap",
        );
    }

    /// The arm a clash leaves is still a table arm: the cfg seats an
    /// out-of-range `Single` as a tail call but defers an out-of-range arm.
    #[test]
    fn an_isa_clash_leaves_a_one_arm_table_an_arm() {
        let site = mid_insn_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(site, multiple(&[(0x2000, None), (0x3000, None)]));
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[],
            &[pcode_addr(0x3000)],
            &DispatchAnchors::from_addrs([]),
            &mut abandoned,
            &mut known,
        );

        assert_eq!(hit, vec![site]);
        assert_eq!(known[&site], multiple(&[(0x2000, None)]));
    }

    /// A caller can only spell the machine address, so a clash on its seat has
    /// to be reported and frozen under the `BRANCHIND`'s own anchor.
    #[test]
    fn an_isa_clash_on_a_machine_start_seed_is_keyed_to_the_anchor() {
        let start = pcode_addr(0x1000);
        let anchor = mid_insn_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(start, multiple(&[(0x2000, None), (0x3000, None)]));
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[],
            &[pcode_addr(0x3000)],
            &DispatchAnchors::from_addrs([anchor]),
            &mut abandoned,
            &mut known,
        );

        assert_eq!(hit, vec![anchor]);
        assert!(
            abandoned.contains(&anchor),
            "`apply_resolutions` tests this set with the anchor, so a site \
             frozen only under the seed key takes the next round's fold",
        );
        assert!(abandoned.contains(&start));
    }

    #[test]
    fn an_isa_clash_on_every_arm_abandons_the_site() {
        let site = pcode_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(
            site,
            multiple(&[(0x3000, Some(false)), (0x3000, Some(true))]),
        );
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[],
            &[pcode_addr(0x3000)],
            &DispatchAnchors::from_addrs([]),
            &mut abandoned,
            &mut known,
        );

        assert_eq!(hit, vec![site]);
        assert!(!known.contains_key(&site));
    }

    #[test]
    fn a_site_naming_no_bad_target_is_left_alone() {
        let site = mid_insn_addr(0x1000);
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(site, multiple(&[(0x2000, None)]));
        known.insert(pcode_addr(0x5000), ResolvedTargets::LinkRegister);
        let mut abandoned = rustc_hash::FxHashSet::default();

        let mut known = Cow::Owned(known);
        let hit = abandon_undecodable(
            &[undecodable(pcode_addr(0x7000), 0x9000)],
            &[pcode_addr(0x8000)],
            &DispatchAnchors::from_addrs([]),
            &mut abandoned,
            &mut known,
        );

        assert!(hit.is_empty());
        assert_eq!(known.len(), 2);
        assert!(abandoned.is_empty());
    }

    /// One fold at a `BRANCHIND` anchored mid-instruction, with the previous
    /// round's mode-bearing answer seated under `known_key`.
    fn fold_mid_insn(known_key: PcodeInsnAddr) -> (BTreeSet<TargetKey>, Progress) {
        let (_function, node) = fn_with_live_indirect_branch();
        let anchor = mid_insn_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(anchor, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(known_key, multiple(&[(0x2000, Some(true))]));
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(multiple(&[(0x2000, None), (0x3000, None)])));
        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &Vec::new(),
            &anchors,
            resolutions,
            &FxHashMap::default(),
            &rustc_hash::FxHashSet::default(),
        )
        .expect("fold");
        (target_keys(&known[&anchor]), progress)
    }

    /// A caller can only spell the machine address, so at a site whose
    /// `BRANCHIND` is not the instruction's first p-code op the mode-bearing
    /// answer is seated under the machine-start key. Adoption must read it
    /// there: an exact-key miss adopts nothing, and the interworking arm the
    /// anchor-keyed fold drops is seated mode-less with every report empty.
    #[test]
    fn apply_resolutions_adopts_a_mode_seated_under_the_machine_start_key() {
        let (anchored, anchored_progress) = fold_mid_insn(mid_insn_addr(0x1000));
        let (machine_start, machine_start_progress) = fold_mid_insn(pcode_addr(0x1000));

        assert_eq!(
            anchored,
            target_keys(&multiple(&[(0x2000, Some(true))])),
            "the mode-less 0x3000 must not be seated at an interworking site",
        );
        assert_eq!(
            anchored_progress.derived_incomplete,
            vec![mid_insn_addr(0x1000)]
        );
        assert_eq!(machine_start, anchored);
        assert_eq!(
            machine_start_progress.derived_incomplete,
            anchored_progress.derived_incomplete,
        );
        assert_eq!(
            machine_start_progress.assumed_modes,
            anchored_progress.assumed_modes,
        );
    }

    /// A frozen site takes no further classification, so the arms
    /// `abandon_undecodable` cut cannot be folded back in.
    #[test]
    fn an_abandoned_site_folds_nothing() {
        let (_function, node) = fn_with_live_indirect_branch();
        let addr = pcode_addr(0x1000);
        let anchors: UnresolvedAnchors = vec![(addr, node)];
        let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
        known.insert(addr, multiple(&[(0x2000, None)]));
        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(multiple(&[(0x2000, None), (0x3000, None)])));
        let abandoned: rustc_hash::FxHashSet<PcodeInsnAddr> = std::iter::once(addr).collect();

        let progress = apply_resolutions(
            &mut known,
            &FxHashMap::default(),
            &anchors,
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &abandoned,
        )
        .expect("fold");

        assert!(!progress.changed());
        assert_eq!(
            target_keys(&known[&addr]),
            target_keys(&multiple(&[(0x2000, None)])),
        );
    }

    /// Abandoning one dispatch of a two-dispatch instruction drops the shared
    /// machine-start key, and the other dispatch's next fold re-seats the
    /// caller's answer under its own anchor.
    #[test]
    fn abandoning_one_dispatch_keeps_the_caller_seed_of_its_neighbour() {
        let (_function, node) = fn_with_live_indirect_branch();
        let start = pcode_addr(0x1000);
        let first = mid_insn_addr(0x1000);
        let second = PcodeInsnAddr {
            insn_index: 5,
            ..first
        };
        let caller_seed: FxHashMap<PcodeInsnAddr, ResolvedTargets> =
            std::iter::once((start, multiple(&[(0x4000, None)]))).collect();
        let mut known = caller_seed.clone();
        let mut abandoned = rustc_hash::FxHashSet::default();
        abandon_site(&mut known, &mut abandoned, first);
        assert!(
            known.is_empty(),
            "the shape under test: the shared key is gone"
        );

        let mut resolutions: IndirectResolutions = FxHashMap::default();
        resolutions.insert(node, Some(multiple(&[(0x5000, None)])));
        apply_resolutions(
            &mut known,
            &caller_seed,
            &vec![(second, node)],
            &Vec::new(),
            resolutions,
            &FxHashMap::default(),
            &abandoned,
        )
        .expect("fold");

        assert_eq!(
            target_keys(&known[&second]),
            target_keys(&multiple(&[(0x4000, None), (0x5000, None)])),
        );
    }
}
