pub use strider_opt as opt;

pub use strider_cfg::{Incompleteness, Reason};
pub use strider_lift::lift::{LiftOutcome, Lifter};

mod resolve;

#[cfg(doctest)]
#[doc = include_str!("../tests/rewrite_rhs_refusals.md")]
struct RewriteRhsRefusals;

#[cfg(feature = "test-util")]
pub use resolve::Classifier;

use rustc_hash::FxHashMap;

use anyhow::{Result, anyhow};

use strider_cfg::MachineInsnAddr;
use strider_opt::{AssumptionOptions, ReadOnlyMemory};

/// What `Default` lends: a `&'a` cannot borrow a temporary.
static NO_PER_ADDRESS_CCS: FxHashMap<u64, strider_target::BuiltCallingConvention> =
    FxHashMap::with_hasher(rustc_hash::FxBuildHasher);

/// [`Strider::analyze`]'s options. `Default` decodes an unbounded function
/// with no seeds and no CC overrides, optimises under
/// [`AssumptionOptions::default`], resolves indirect branches and compacts.
#[derive(Clone, Debug)]
pub struct AnalyzeOptions<'a> {
    /// Its `derived_targets` must be empty: [`Strider::analyze`] fills that
    /// layer with its own answers.
    pub cfg: strider_cfg::CfgOptions<'a>,
    /// Keyed by direct-call target address, through `cfg`'s
    /// [`strider_cfg::CfgOptions::local_entries`], read by the lift only. Built
    /// against this engine's registers and fitting its arch, as the default CC
    /// must ([`Lifter::build_ir_with`]); borrowed, as
    /// [`strider_cfg::CfgOptions::known_targets`] is.
    pub per_address_ccs: &'a FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// Run the indirect-branch classifier.  Off leaves every unseeded site an
    /// `IndirectBranch` placeholder, so a caller can hand its own answers in
    /// through `CfgOptions::known_targets` instead, or see the raw dispatch
    /// shape when a resolution looks wrong.  Caller-supplied targets still
    /// seat: they are read by the CFG builder, not the classifier.
    pub resolve_indirect_branches: bool,
    /// The claims each round's fresh lift holds
    /// ([`strider_ir::Function::assumptions`]): the lift reads them, as the
    /// optimizer does.
    pub assumptions: AssumptionOptions,
    /// Drops unreachable nodes once the loop settles.
    pub compact: bool,
}

impl Default for AnalyzeOptions<'_> {
    fn default() -> Self {
        Self {
            cfg: strider_cfg::CfgOptions::default(),
            per_address_ccs: &NO_PER_ADDRESS_CCS,
            resolve_indirect_branches: true,
            assumptions: AssumptionOptions::default(),
            compact: true,
        }
    }
}

/// Per-binary analysis handle.
pub struct Strider<R>
where
    R: rsleigh::MemReader,
{
    lifter: Lifter<R>,
    /// The read-only image `LoadReadOnly` folds against and the indirect-branch
    /// classifier reads jump tables from; `None` disables both.
    rom: Option<Box<dyn ReadOnlyMemory>>,
}

impl<R> Strider<R>
where
    R: rsleigh::MemReader,
{
    /// A handle decoding through `reader`, with the Sleigh built from `arch`.
    ///
    /// # Errors
    ///
    /// Returns `Err` from [`Lifter::new`]: the sla not loading,
    /// `Sleigh::user_op_names()` failing, the arch
    /// declaring no address space with shortcut `'r'` or naming that space
    /// something other than `"ram"`, and `DecodeEngine::new` failing. An arch
    /// and sla that disagree on the ISA mode (a mode var the sla does not
    /// flow, an unpaired `ISAModeSwitch`) fail too.
    pub fn new(
        arch: strider_target::SleighArch,
        reader: R,
        rom: Option<Box<dyn ReadOnlyMemory>>,
    ) -> Result<Self> {
        let lifter = Lifter::new(arch, reader)
            .map_err(|e| anyhow!("Strider::new: Lifter::new failed: {e:?}"))?;
        Ok(Self { lifter, rom })
    }

    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lifter.sleigh_regs()
    }

    #[must_use]
    pub fn arch(&self) -> strider_target::SleighArch {
        self.lifter.arch()
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
    /// Propagates CFG-build failures (see [`Lifter::build_cfg`]).
    pub fn build_cfg(
        &mut self,
        entry: u64,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        self.lifter
            .build_cfg(MachineInsnAddr::from(entry), cfg_opts)
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
    /// Returns even when it cannot vouch for every path: check
    /// [`AnalyzeResult::incomplete`] before trusting an absence ("no store to
    /// X"), since code behind a listed address may be missing. An unresolved
    /// site keeps its placeholder in the function. A site the CFG consumed as
    /// a `Return` or `TailCall` leaves no placeholder, so it is
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
    /// not, and a `LinkRegister` seed reports a p-code `RETURN` it covers whose
    /// target is unproven as [`Reason::UnverifiedSeed`] instead; it never
    /// suppresses what the classifier derives. `opts.compact` applies once,
    /// after the loop.
    ///
    /// `pipeline` defaults to [`strider_opt::default_pipeline`]. Every round
    /// optimizes a fresh lift under `opts.assumptions` and classifies its
    /// dispatch sites with [`strider_opt::classify_sites`] after the pipeline,
    /// whatever pipeline it is; with `opts.resolve_indirect_branches` off it
    /// lists them unresolved instead ([`strider_opt::ClassifiedSites::unresolved`]).
    ///
    /// # Errors
    ///
    /// A non-empty `opts.cfg.derived_targets`, which is the loop's own layer;
    /// lift / cfg / opt / validation failures; never an incomplete result.
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
    ) -> Result<AnalyzeResult> {
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
    ) -> Result<AnalyzeResult> {
        self.analyze_with(entry, cc, opts, pipeline, classify)
    }

    fn analyze_with(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
        classify: resolve::Classifier<'_>,
    ) -> Result<AnalyzeResult> {
        anyhow::ensure!(
            opts.cfg.derived_targets.is_empty(),
            "analyze keeps its own answers in derived_targets; seed through \
             known_targets instead"
        );
        let pipeline = pipeline.unwrap_or_else(strider_opt::default_pipeline);
        let mut result =
            self.resolve(MachineInsnAddr::from(entry), cc, opts, &pipeline, classify)?;
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
}
