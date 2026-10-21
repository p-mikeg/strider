//! The Sleigh engine and the decode context it carries from one function to
//! the next.

use rustc_hash::FxHashMap;

use crate::builder::{Builder, FlowCommit, FlowContext, FlowVars};
use crate::{Cfg, CfgOptions, MachineInsnAddr, Result};

/// A Sleigh engine and the decode context it carries across functions, so
/// each [`Self::build_cfg`] decodes as a fresh engine would.
///
/// `Sleigh` keeps every `globalset` per address, past the build that made it.
/// The engine pins each entry back to the context its ISA mode last produced,
/// and every build re-derives the committed vars from its own decode and
/// resets the `noflow` vars that change one.
///
/// ```no_run
/// use rsleigh::mem_readers::BufMemReader;
/// use strider_cfg::{CfgOptions, DecodeEngine};
/// use strider_target::SleighArch;
///
/// let arch = SleighArch::x86_64();
/// let reader = BufMemReader::new(vec![0xc3], 0x1000);
/// let sleigh =
///     rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
/// let mut engine = DecodeEngine::new(arch, sleigh)?;
/// let cfg = engine.build_cfg(0x1000u64.into(), &CfgOptions::default())?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct DecodeEngine<R: rsleigh::MemReader> {
    pub(crate) sleigh: rsleigh::Sleigh<R>,
    pub(crate) arch: strider_target::SleighArch,
    /// The sla's flowing context vars.
    pub(crate) flow_vars: FlowVars,
    /// The flow context of a cold entry on a fresh engine: the pspec defaults,
    /// read before any decode.
    entry_defaults: FlowContext,
    /// The pspec default of each `noflow` var that changes a decode
    /// ([`strider_target::SleighArch::transient_decode_vars`]). `flow_vars`
    /// excludes them by definition.
    pub(crate) transient_defaults: Vec<(&'static str, u32)>,
    /// Each [`strider_target::SleighArch::flow_commit_vars`] var with its flow
    /// class and pspec default. A default is committed globally, so any
    /// address reads the same value.
    pub(crate) flow_commit_defaults: Vec<(&'static str, FlowCommit, u32)>,
    /// Indexed by `user_op_id`.
    pub(crate) user_op_names: Vec<String>,
    /// The entry context each ISA mode last pinned, keyed by the mode value
    /// ([`strider_target::SleighArch::entry_mode_context`]), `None` for an
    /// arch without one.
    entry_contexts: FxHashMap<Option<u32>, FlowContext>,
}

impl<R: rsleigh::MemReader> DecodeEngine<R> {
    /// # Errors
    ///
    /// When the sla's user-op names or flowing context vars cannot be read,
    /// the sla does not flow the arch's ISA-mode var, it does not declare a
    /// var the arch re-derives, or `sleigh` was built from another sla or
    /// pspec than `arch`'s.
    pub fn new(arch: strider_target::SleighArch, sleigh: rsleigh::Sleigh<R>) -> Result<Self> {
        // Not `unwrap_or_default`: with an empty table the CFG classifies every
        // `CallOther` as returning, so a no-return one decodes on into whatever
        // follows it, and the lift rejects every one as an unknown user-op.
        let user_op_names = sleigh.user_op_names()?;
        let flow_vars = FlowVars::discover(&sleigh)?;
        // Every region re-imposes its mode through the flowing set; a mode var
        // the sla does not flow would make that a silent no-op.
        if let Some(var) = arch.isa_mode_var()
            && !flow_vars.contains(var)
        {
            anyhow::bail!(
                "{arch:?}: ISA-mode var {var} is not among the sla's flowing context vars"
            );
        }
        // Read on the still-fresh engine, so this is the pspec default, not a
        // leak.  The address is immaterial: a flowing var's default is committed
        // globally, so any address reads the same value.
        let entry_defaults = flow_vars.snapshot(&sleigh, 0);
        // A hard error, unlike `transient_defaults` below: a named var the sla
        // does not declare means this preset's list is wrong, and silently
        // dropping it would leave the leak it exists to close.
        let flow_commit_defaults = arch
            .flow_commit_vars()
            .iter()
            .map(|&name| {
                let class = flow_vars.commit_class(name);
                Ok((name, class, sleigh.get_context_at(0, name)?))
            })
            .collect::<Result<_>>()?;
        let transient_defaults = arch
            .transient_decode_vars()
            .iter()
            .filter_map(|name| Some((*name, sleigh.get_context_at(0, name).ok()?)))
            .collect();
        // After the checks above, which report the mismatches they can see.
        // The right preset's specs are pointer-equal, costing no memcmp.
        let (sla, want_sla) = (sleigh.sla_spec().0, arch.sla_spec().0);
        anyhow::ensure!(
            std::ptr::eq(sla, want_sla) || sla == want_sla,
            "{:?}: the Sleigh was built from another sla",
            arch.preset()
        );
        // The pspec separates presets that share an sla (`arm`, `arm_thumb`).
        let (pspec, want_pspec) = (sleigh.pspec().set_vars, arch.pspec().set_vars);
        anyhow::ensure!(
            std::ptr::eq(pspec, want_pspec) || pspec == want_pspec,
            "{:?}: the Sleigh was built from another pspec",
            arch.preset()
        );
        Ok(Self {
            sleigh,
            arch,
            flow_vars,
            entry_defaults,
            transient_defaults,
            flow_commit_defaults,
            user_op_names,
            entry_contexts: FxHashMap::default(),
        })
    }

    /// The CFG of the function at `entry`. On an arch with an ISA-mode var the
    /// entry's low bit selects the mode (Thumb, MIPS16e) and the function
    /// decodes at `entry & !1`. Never errs on incompleteness:
    /// [`Cfg::incomplete`] names what it cannot vouch for.
    pub fn build_cfg(&mut self, entry: MachineInsnAddr, opts: &CfgOptions) -> Result<Cfg> {
        self.builder(entry, opts)?.build()
    }

    /// The entry pinned, and a builder over this engine.
    pub(crate) fn builder<'a>(
        &'a mut self,
        entry: MachineInsnAddr,
        opts: &'a CfgOptions<'a>,
    ) -> Result<Builder<'a, R>> {
        // A prior function's `globalset` holds forward until the next change
        // point and leaks into this cold entry on a reused engine: pin the
        // entry back to the pspec defaults captured on the fresh engine.
        let entry_mode = self.arch.entry_mode_context(entry.addr);
        let decode_addr = if entry_mode.is_some() {
            entry.addr & !1
        } else {
            entry.addr
        };
        // The pin target is the context this mode last produced, not the bare
        // defaults: several sla vars view the ISA-mode bit (ARM `TMode`, `T`,
        // `LowBitCodeMode`, `ISA_MODE`), so defaults-then-mode-bit ends on a
        // context the defaults do not describe, and re-pinning the defaults
        // rewrites every view of the bit on every call.  A `set_context_at` is
        // logged for `Sleigh::clone` to replay and the log is bounded, so
        // re-analysing one entry must cost no commit.
        let mode_key = entry_mode.map(|(_, value)| value);
        let want = self
            .entry_contexts
            .get(&mode_key)
            .unwrap_or(&self.entry_defaults);
        self.flow_vars.pin_at(&mut self.sleigh, decode_addr, want)?;
        // The pin's diff can write an alias of the mode bit last, flipping it
        // back.
        if let Some((var, value)) = entry_mode
            && self.sleigh.get_context_at(decode_addr, var)? != value
        {
            self.sleigh.set_context_at(decode_addr, var, value)?;
        }
        // The function's now-committed ISA mode, the base context the builder
        // decodes a strider-resolved target in.
        let function_mode = self.flow_vars.snapshot(&self.sleigh, decode_addr);
        self.entry_contexts.insert(mode_key, function_mode.clone());
        Ok(Builder::new(self, decode_addr, function_mode, opts))
    }

    #[must_use]
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R> {
        &self.sleigh
    }

    #[must_use]
    pub fn arch(&self) -> strider_target::SleighArch {
        self.arch
    }

    /// Indexed by `user_op_id`.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        &self.user_op_names
    }
}
