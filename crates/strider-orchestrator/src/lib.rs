pub use strider_opt as opt;

pub use strider_cfg::{Incompleteness, Reason};
pub use strider_lift::lift::{LiftOutcome, Lifter};

mod resolve;

#[cfg(feature = "test-util")]
pub use resolve::Classifier;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::MachineInsnAddr;
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
    /// unstable answer: it is withdrawn.
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
    /// `pipeline` defaults to [`strider_opt::default_pipeline`]. Every round
    /// classifies the function's dispatch sites with
    /// [`strider_opt::classify_sites`] after the pipeline, whatever pipeline
    /// it is.
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
        self.analyze_with(entry, cc, opts, pipeline, &strider_opt::classify_sites)
    }

    /// [`Self::analyze`], classifying every round's dispatch sites with
    /// `classify` in place of [`strider_opt::classify_sites`].
    ///
    /// # Errors
    ///
    /// As [`Self::analyze`].
    #[cfg(feature = "test-util")]
    pub fn analyze_with_classifier(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
        classify: Classifier<'_>,
    ) -> std::result::Result<AnalyzeResult, AnalyzeError> {
        self.analyze_with(entry, cc, opts, pipeline, classify)
    }

    fn analyze_with(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
        classify: resolve::Classifier<'_>,
    ) -> std::result::Result<AnalyzeResult, AnalyzeError> {
        let pipeline = pipeline.unwrap_or_else(strider_opt::default_pipeline);
        let mut result =
            self.resolve(MachineInsnAddr::from(entry), cc, opts, &pipeline, classify)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The cause chain, as `{:#}` prints it: callers format this error, and it
    /// offers no `source()` to walk.
    #[test]
    fn a_failure_displays_its_whole_chain() {
        let e = AnalyzeError::from(anyhow!("inner").context("outer"));
        assert_eq!(e.to_string(), "outer: inner");
        assert!(std::error::Error::source(&e).is_none());
    }
}
