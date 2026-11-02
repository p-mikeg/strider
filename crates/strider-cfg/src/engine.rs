//! The Sleigh engine and the decode context it carries from one function to
//! the next.

use rustc_hash::FxHashMap;

use crate::builder::{Builder, FlowCommit, FlowContext, FlowVars, hold_var};
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
/// let mut engine = DecodeEngine::new(arch, reader)?;
/// let cfg = engine.build_cfg(0x1000u64.into(), &CfgOptions::default())?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct DecodeEngine<R: rsleigh::MemReader> {
    pub(crate) sleigh: rsleigh::Sleigh<R>,
    pub(crate) arch: strider_target::SleighArch,
    pub(crate) tables: &'static strider_target::DecodeTables,
    /// The sla's flowing context vars.
    pub(crate) flow_vars: FlowVars,
    /// [`strider_target::DecodeTables::flow_commit`], per engine so a test can
    /// lend another.
    pub(crate) flow_commit: &'static [(&'static str, FlowCommit, u32)],
    /// The entry context each ISA mode last pinned, keyed by the mode value
    /// ([`strider_target::SleighArch::entry_mode_context`]), `None` for an
    /// arch without one.
    entry_contexts: FxHashMap<Option<u32>, FlowContext>,
}

impl<R: rsleigh::MemReader> DecodeEngine<R> {
    /// An engine over `reader`, with the Sleigh built from `arch`.
    ///
    /// # Errors
    ///
    /// When the sla does not load.
    pub fn new(arch: strider_target::SleighArch, reader: R) -> Result<Self> {
        let tables = arch.decode_tables();
        Ok(Self {
            sleigh: arch.sleigh(reader)?,
            arch,
            tables,
            flow_vars: FlowVars::of(arch),
            flow_commit: &tables.flow_commit,
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
        let tables = self.tables;
        // A cold mode pins the pspec defaults.
        let want = self
            .entry_contexts
            .entry(mode_key)
            .or_insert_with(|| FlowContext::from(&tables.entry_defaults[..]));
        self.flow_vars.pin_at(&mut self.sleigh, decode_addr, want)?;
        // The pin's diff can write an alias of the mode bit last, flipping it
        // back.
        if let Some((var, value)) = entry_mode {
            hold_var(&mut self.sleigh, decode_addr, var, value)?;
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

    #[must_use]
    pub fn tables(&self) -> &'static strider_target::DecodeTables {
        self.tables
    }

    /// Indexed by `user_op_id`.
    #[must_use]
    pub fn user_op_names(&self) -> &[String] {
        &self.tables.user_op_names
    }
}
