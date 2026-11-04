use rustc_hash::FxHashMap;

use crate::builder::{Builder, FlowContext, FlowVars, hold_var};
use crate::{Cfg, CfgOptions, MachineInsnAddr, Result};

/// A Sleigh engine and the decode context it carries across functions.
///
/// `Sleigh` keeps every `globalset` past the build that made it, so each
/// entry is pinned back to its ISA mode's context and each build re-derives
/// its own commits: every [`Self::build_cfg`] decodes as a fresh engine would.
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
    /// The sla's address spaces, fixed for the engine's lifetime.
    space_ids: rsleigh::SpaceIds,
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
    ///
    /// # Panics
    ///
    /// When the preset and its sla disagree
    /// ([`strider_target::SleighArch::decode_tables`]).
    pub fn new(arch: strider_target::SleighArch, reader: R) -> Result<Self> {
        // Before the tables, whose own Sleigh panics where this one errs.
        let sleigh = arch.sleigh(reader)?;
        let tables = arch.decode_tables();
        Ok(Self {
            space_ids: sleigh.space_ids(),
            sleigh,
            arch,
            tables,
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
        let entry_mode = self.arch.entry_mode_context(entry.addr);
        let decode_addr = if entry_mode.is_some() {
            entry.addr & !1
        } else {
            entry.addr
        };
        // Pinned to the context this mode last produced, not the defaults:
        // several vars alias the mode bit, and re-pinning defaults would
        // commit on every call to a bounded replay log.
        let mode_key = entry_mode.map(|(_, value)| value);
        let tables = self.tables;
        // A cold mode pins the pspec defaults.
        let want = self
            .entry_contexts
            .entry(mode_key)
            .or_insert_with(|| FlowContext::from(&tables.entry_defaults[..]));
        FlowVars::of(tables).pin_at(&mut self.sleigh, decode_addr, want)?;
        // The pin can write an alias of the mode bit last, flipping it.
        if let Some((var, value)) = entry_mode {
            hold_var(&mut self.sleigh, decode_addr, var, value)?;
        }
        let function_mode = self.flow_vars().snapshot(&self.sleigh, decode_addr);
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

    /// What every [`Cfg::space_ids`] this engine builds holds.
    #[must_use]
    pub fn space_ids(&self) -> &rsleigh::SpaceIds {
        &self.space_ids
    }

    /// The sla's flowing context vars.
    pub(crate) fn flow_vars(&self) -> FlowVars {
        FlowVars::of(self.tables)
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
