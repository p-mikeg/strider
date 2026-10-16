pub use strider_opt as opt;

pub use strider_cfg::{Incompleteness, Reason};
pub use strider_lift::lift::{LiftOutcome, Lifter};

mod resolve;

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::{MachineInsnAddr, ResolvedTargets};
use strider_opt::{OptOptions, ReadOnlyMemory};

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
    /// Refuse it: [`AnalyzeError::Incomplete`] names the incomplete set.
    #[default]
    Error,
    /// Return it: its [`Incompleteness`] names the incomplete set.
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
    /// The analysis finished but cannot vouch for this incomplete set, and
    /// [`OnIncomplete::Error`] refused it.
    Incomplete(Incompleteness),
    /// Decode, lift, optimisation or validation failed.
    Failed(anyhow::Error),
}

impl std::fmt::Display for AnalyzeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalyzeError::Incomplete(incomplete) => write!(f, "incomplete analysis: {incomplete}"),
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
    /// branches against the optimised IR, folds the targets into the seats the
    /// next round's cfg is built from and re-lifts, until the induced edge set
    /// stops changing.
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
    /// `opts.cfg.known_targets` is borrowed and never copied or mutated: the
    /// loop keeps its own answers in a layer above it
    /// ([`strider_cfg::CfgOptions::derived_targets`]), and a site it gives up
    /// on is withdrawn there, which defers it over the caller's seed. A seed
    /// suppresses the unresolved report of an unclassifiable seated `Switch` at
    /// its address only while the settled answer names nothing the seed did
    /// not; it never suppresses what the classifier derives. `opts.compact`
    /// applies once, after the loop.
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
        let mut result = self.resolve(MachineInsnAddr::from(entry), cc, opts, pipeline)?;
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

/// `pipeline` running [`strider_opt::IndirectBranchClassify`], at most once: a
/// caller-supplied pipeline that already registers it would otherwise repeat
/// the pass's known-bits / dominator / value-range setup every re-lift round.
fn with_classify(mut pipeline: strider_opt::OptimizerPipeline) -> strider_opt::OptimizerPipeline {
    if !pipeline.has_post_pass::<strider_opt::IndirectBranchClassify>() {
        pipeline.add_post_pass(strider_opt::IndirectBranchClassify);
    }
    pipeline
}

/// `targets` with each mode-less entry taking the ISA mode `known` already
/// proved for that address.
///
/// At a site `known` proves a mode DIFFERENT from `flowing_isa_bit`, an
/// address only a mode-less re-derivation reports is dropped rather than
/// seated: `None` reads to the cfg as the mode flowing into the branch, which
/// at an interworking dispatch is the mode being switched away from. The
/// dropped set stays seatable, so the caller reports the site through
/// `Progress::derived_incomplete` rather than relying on the cfg to re-defer
/// it.
/// Where every proved mode IS the flowing one, seating `None` decodes
/// identically, so the set still widens. Only the arms already evaluated
/// proved that mode, and they are no evidence about an arm nobody has
/// evaluated, so a later round widening the index can seat an address whose
/// committed mode was never checked. Dropping it would cost every same-mode
/// dispatch its arms, so it is kept and the second return value reports the
/// site through `Progress::assumed_modes`: the arm set is whole, which is an
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
/// bounds are stated in. Every target-slice read in the fold must go through
/// it, or the cost tests cannot see that read.
fn visit(
    side: &[strider_cfg::ResolvedTarget],
) -> impl Iterator<Item = &strider_cfg::ResolvedTarget> + '_ {
    side.iter().inspect(|_| {
        #[cfg(test)]
        crate::resolve::test_support::TARGET_VISITS.with(|v| v.set(v.get() + 1));
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
/// `SiteTable::fold` accounts for it: `None` is one of the successors
/// `reported` tracks, so the site comes back in `Progress::derived_incomplete`
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
            // CFG cannot hold both, so drop it; the fold reports
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

/// One address's induced successor set, the unit the convergence check
/// compares. `target = None` is a `LinkRegister` resolution.
fn target_keys(resolved: &ResolvedTargets) -> BTreeSet<TargetKey> {
    targets_of(resolved).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::test_support::{assert_linear_visits, multiple};

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
}
