use anyhow::Result;

pub use strider_lift::lift::{LiftOptions, LiftOutcome};

/// Architecture-level lift + optimize driver.
///
/// Wraps a [`strider_lift::lift::Lifter`] (the CFG→IR lift surface) and
/// adds the orchestrator's optimization concern: the
/// [`strider_opt::AliasMode`] used by the SP-aware pipelines and the
/// pipeline-builder helper.  `LiftDriver` is the internal handle behind
/// the Python `strider.Strider` class (`PyStrider`)'s single-lift
/// `analyze_cfg` surface, so the lift + opt surface lives in one place.
/// (The top-level run loop lives on [`crate::orchestrator::Strider`],
/// which builds its own `Lifter` directly rather than wrapping a
/// `LiftDriver`.)  Lift calls forward to the wrapped `Lifter`.
///
/// `Clone` copies the wrapped `Lifter` (resolved calling convention +
/// cached `SleighRegs` table) and the alias mode.  The cached `SleighRegs`
/// table isn't free to clone, but far cheaper than re-running the
/// "expensive" `Sleigh::regs()` to rebuild it.  The strider-py `run` path
/// uses this to detach a snapshot from a `PyRef` so it can release the GIL
/// across `strider::run` (otherwise Python threads would be unable to make
/// progress while a long lift / fixed-point loop runs).
#[derive(Clone)]
pub struct LiftDriver {
    pub(crate) lifter: strider_lift::lift::Lifter,
    /// Alias-analysis precision propagated to every SP-aware pass the
    /// pipeline builders construct.  Default is
    /// [`strider_opt::AliasMode::StackGlobalDisjoint`].
    pub(crate) alias_mode: strider_opt::AliasMode,
}

impl LiftDriver {
    /// Creates a new `LiftDriver` for `arch` with the given Sleigh register list
    /// and calling convention.
    ///
    /// Resolves all register names in `calling_convention` against
    /// `sleigh_regs`.
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` if any register name in
    /// `calling_convention` (including the stack pointer) does not resolve
    /// against `sleigh_regs`.
    pub fn new(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::CallingConvention,
    ) -> Result<Self> {
        Ok(Self {
            lifter: strider_lift::lift::Lifter::new(arch, sleigh_regs, calling_convention)?,
            alias_mode: strider_opt::AliasMode::default(),
        })
    }

    /// Constructs a `LiftDriver` from an already-resolved
    /// `BuiltCallingConvention`.  Use this when the CC was built
    /// outside the standard preset path (e.g. a custom CC constructed
    /// from runtime register-name lists at the Python boundary).
    ///
    /// Unlike [`Self::new`], no name resolution runs — the caller is
    /// responsible for ensuring `calling_convention`'s varnodes resolve
    /// against `sleigh_regs`.  ABI invariants are pinned at
    /// [`strider_target::BuiltCallingConvention::try_new`] construction
    /// time; this constructor trusts that contract.
    #[must_use]
    pub fn from_built_cc(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::BuiltCallingConvention,
    ) -> Self {
        Self {
            lifter: strider_lift::lift::Lifter::from_built_cc(arch, sleigh_regs, calling_convention),
            alias_mode: strider_opt::AliasMode::default(),
        }
    }

    /// Returns the resolved calling convention this `LiftDriver` was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        self.lifter.calling_convention()
    }

    /// Overrides the [`strider_opt::AliasMode`] propagated to the
    /// SP-aware passes via the shared [`strider_opt::OptCtx`].
    #[must_use]
    pub const fn with_alias_mode(mut self, mode: strider_opt::AliasMode) -> Self {
        self.alias_mode = mode;
        self
    }

    /// Returns the [`strider_opt::AliasMode`] this driver propagates to the
    /// SP-aware passes.  The orchestrator reads it to seed the shared
    /// [`strider_opt::OptCtx::alias_mode`] once per pipeline run.
    #[must_use]
    pub const fn alias_mode(&self) -> strider_opt::AliasMode {
        self.alias_mode
    }

    /// Returns the default optimizer pipeline
    /// ([`strider_opt::default_pipeline`]) — now the full set, including the
    /// SP-aware passes (`LoadForward`, `StackOffsetDetect`,
    /// `CallStackArgCollect`, `FunctionArgDetect`).  Those read their
    /// calling convention from the function's `default_cc` and their alias
    /// precision from the per-run [`strider_opt::OptCtx`], so this driver
    /// adds nothing beyond the default.
    #[must_use]
    pub fn build_optimizer_pipeline(&self) -> strider_opt::OptimizerPipeline {
        strider_opt::default_pipeline()
    }

    /// Translates a complete control-flow graph into a [`LiftOutcome`].
    ///
    /// Forwards to [`strider_lift::lift::Lifter::analyze_cfg`].
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` when the CFG is malformed (missing
    /// region, unknown terminator), instruction translation fails (an
    /// unsupported opcode or varnode), or IR validation fails.
    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self,
        cfg: &strider_cfg::Cfg,
        sleigh: &rsleigh::Sleigh<R>,
    ) -> Result<LiftOutcome> {
        self.lifter.analyze_cfg(cfg, sleigh)
    }

    /// Translates a complete CFG into a [`LiftOutcome`] with
    /// caller-supplied [`LiftOptions`].
    ///
    /// Forwards to [`strider_lift::lift::Lifter::analyze_cfg_with`].
    ///
    /// # Errors
    ///
    /// Propagates errors from the lift (variable-table init, CC build,
    /// per-region IR translation, and final IR validation).
    pub fn analyze_cfg_with<R: rsleigh::MemReader>(
        &self,
        cfg: &strider_cfg::Cfg,
        sleigh: &rsleigh::Sleigh<R>,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        self.lifter.analyze_cfg_with(cfg, sleigh, opts)
    }
}
