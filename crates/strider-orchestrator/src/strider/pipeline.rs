use anyhow::Result;

pub use strider_lift::lift::{LiftOptions, LiftOutcome};

/// Architecture-level lift + optimize driver.
///
/// Wraps the owning [`strider_lift::lift::Lifter<R>`] (which owns the
/// `Sleigh<R>`) and adds the orchestrator's optimization concern: the
/// [`strider_opt::AliasMode`] used by the SP-aware pipelines.
/// `LiftDriver` is the internal handle behind the Python `strider.Lifter`
/// class (`PyLifter`)'s `build_cfg` + `analyze_cfg` surface, so the lift +
/// opt surface lives in one place.  (The top-level run loop lives on
/// [`crate::orchestrator::Strider`], which owns its own `Lifter`
/// directly.)  Lift calls forward to the wrapped `Lifter`; the calling
/// convention is a per-call argument (the engine does not store it).
pub struct LiftDriver<R: rsleigh::MemReader> {
    pub(crate) lifter: strider_lift::lift::Lifter<R>,
    /// Alias-analysis precision propagated to every SP-aware pass the
    /// pipeline builders construct.  Default is
    /// [`strider_opt::AliasMode::StackGlobalDisjoint`].
    pub(crate) alias_mode: strider_opt::AliasMode,
}

impl<R: rsleigh::MemReader> LiftDriver<R> {
    /// Creates a `LiftDriver` for `arch` owning `sleigh` (via
    /// [`strider_lift::lift::Lifter::new`], which caches the register
    /// table).  The calling convention is supplied per lift call.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `Sleigh::regs()` fails.
    pub fn new(
        arch: strider_target::SleighArch,
        sleigh: rsleigh::Sleigh<R>,
    ) -> Result<Self> {
        Ok(Self {
            lifter: strider_lift::lift::Lifter::new(arch, sleigh)?,
            alias_mode: strider_opt::AliasMode::default(),
        })
    }

    /// Returns the target architecture description.
    #[must_use]
    pub fn arch(&self) -> &strider_target::SleighArch {
        self.lifter.arch()
    }

    /// Read access to the owned Sleigh context (dot rendering / pcode).
    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        self.lifter.sleigh()
    }

    /// Returns the cached Sleigh register-name table.
    #[must_use]
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs {
        self.lifter.sleigh_regs()
    }

    /// Builds the CFG for the function at `entry` using the owned Sleigh.
    ///
    /// # Errors
    ///
    /// Propagates CFG build failures.
    pub fn build_cfg(
        &mut self,
        entry: strider_cfg::MachineInsnAddr,
        cfg_opts: &strider_cfg::CfgOptions,
    ) -> Result<strider_cfg::Cfg> {
        self.lifter.build_cfg(entry, cfg_opts)
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
    /// Forwards to [`strider_lift::lift::Lifter::build_ir`].
    ///
    /// # Errors
    ///
    /// Returns an `anyhow::Error` when the CFG is malformed (missing
    /// region, unknown terminator), instruction translation fails (an
    /// unsupported opcode or varnode), or IR validation fails.
    pub fn build_ir(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: &strider_target::BuiltCallingConvention,
    ) -> Result<LiftOutcome> {
        self.lifter.build_ir(cfg, cc)
    }

    /// Translates a complete CFG into a [`LiftOutcome`] with the
    /// function-default `cc` and caller-supplied [`LiftOptions`].
    ///
    /// Forwards to [`strider_lift::lift::Lifter::build_ir_with`].
    ///
    /// # Errors
    ///
    /// Propagates errors from the lift (variable-table init, per-region IR
    /// translation, and final IR validation).
    pub fn build_ir_with(
        &self,
        cfg: &strider_cfg::Cfg,
        cc: &strider_target::BuiltCallingConvention,
        opts: &LiftOptions,
    ) -> Result<LiftOutcome> {
        self.lifter.build_ir_with(cfg, cc, opts)
    }
}
