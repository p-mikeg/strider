pub use strider_opt as opt;

pub use strider_cfg::{Incompleteness, Reason};
pub use strider_lift::{LiftOutcome, LiftSites, Lifter};

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

/// `Default`'s `per_address_ccs`: a `&'a` cannot borrow a temporary.
static NO_PER_ADDRESS_CCS: FxHashMap<u64, strider_target::BuiltCallingConvention> =
    FxHashMap::with_hasher(rustc_hash::FxBuildHasher);

/// [`Strider::analyze`]'s options.
#[derive(Clone, Debug)]
pub struct AnalyzeOptions<'a> {
    /// Its `derived_targets` must be empty: [`Strider::analyze`] fills that
    /// layer with its own answers.
    pub cfg: strider_cfg::CfgOptions<'a>,
    /// Callee CCs by direct-call target, looked up through `cfg`'s
    /// [`strider_cfg::CfgOptions::local_entries`]; built for this engine's
    /// arch, as the default CC must be.
    pub per_address_ccs: &'a FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// Off leaves every unseeded site an `IndirectBranch` placeholder;
    /// `CfgOptions::known_targets` still seat.
    pub resolve_indirect_branches: bool,
    /// The claims each round's lift holds
    /// ([`strider_ir::Function::assumptions`]).
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
    /// Returns `Err` from [`Lifter::new`]: the sla not loading.
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

    /// The only engine that can lift the CFGs `analyze` returns.
    #[must_use]
    pub fn lifter(&self) -> &Lifter<R> {
        &self.lifter
    }

    /// The read-only image `LoadReadOnly` and the jump-table classifier
    /// read; `None` disables both.
    #[must_use]
    pub fn rom(&self) -> Option<&dyn ReadOnlyMemory> {
        self.rom.as_deref()
    }

    /// Structural CFG build only: no lift, optimisation or indirect-branch
    /// resolution.
    ///
    /// # Errors
    ///
    /// As [`Lifter::build_cfg`].
    pub fn build_cfg(
        &mut self,
        entry: u64,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        self.lifter
            .build_cfg(MachineInsnAddr::from(entry), cfg_opts)
    }

    /// Lifts `entry`, runs `pipeline` (default
    /// [`strider_opt::default_pipeline`]) and classifies its dispatch sites,
    /// re-lifting until the seats stop changing or the iteration cap runs out.
    /// `opts.compact` applies once, after the loop.
    ///
    /// # Errors
    ///
    /// A non-empty `opts.cfg.derived_targets`, or a lift, CFG, opt or
    /// validation failure; incompleteness is [`AnalyzeResult::incomplete`].
    pub fn analyze(
        &mut self,
        entry: u64,
        cc: &strider_target::BuiltCallingConvention,
        opts: &AnalyzeOptions<'_>,
        pipeline: Option<strider_opt::OptimizerPipeline>,
    ) -> Result<AnalyzeResult> {
        self.resolve(entry, cc, opts, pipeline, &strider_opt::classify_sites)
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
        self.resolve(entry, cc, opts, pipeline, classify)
    }
}

/// [`Strider::analyze`]'s result.
pub struct AnalyzeResult {
    /// The CFG `function` was lifted from, carrying this analysis's
    /// [`Incompleteness`] rather than that build's own.
    pub cfg: strider_cfg::Cfg,
    /// May still hold an `IndirectBranch` placeholder at a
    /// [`Reason::UnresolvedBranch`] site.
    pub function: strider_ir::Function,
}

impl AnalyzeResult {
    /// What this analysis cannot vouch for: every round's target reasons and
    /// the final round's site reasons.
    #[must_use]
    pub fn incomplete(&self) -> &Incompleteness {
        self.cfg.incomplete()
    }
}
