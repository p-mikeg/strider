//! What an arch's sla says about decoding and lifting: the per-preset facts a
//! decode engine and a lift engine read, built and checked once per process.

use std::ops::Range;

use rsleigh::{MemReader, Sleigh, SleighRegs, Vn, VnSpace};

use crate::arch::SleighArch;

/// Whether the sla declares a committed context var flowing, which decides
/// what a re-derive erases it to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlowCommit {
    /// Confined to one address, so the pspec default is the erase target.
    NoFlow,
    /// Painted forward to persist, so the predecessor's value is.
    Flowing,
}

/// One arch's decode and lift tables, read off a fresh Sleigh.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DecodeTables {
    /// Indexed by `user_op_id`.
    pub user_op_names: Vec<String>,
    /// The context vars the sla marks as flowing.
    pub flow_vars: Vec<String>,
    /// The pspec default of each of `flow_vars`, positionally.
    pub entry_defaults: Vec<u32>,
    /// Each [`SleighArch::flow_commit_vars`] var with its flow class and pspec
    /// default. A default is committed globally, so any address reads it.
    pub flow_commit: Vec<(&'static str, FlowCommit, u32)>,
    /// The pspec default of each [`SleighArch::transient_decode_vars`] var the
    /// sla declares.
    pub transient_defaults: Vec<(&'static str, u32)>,
    /// The declared register file as a slice; `SleighRegs` only hands out an
    /// iterator.
    pub declared_reg_vns: Vec<Vn>,
    /// The register a branch commits its target's ISA mode to (ARM's
    /// `setISAMode` reads it, MIPS's `JXWritePC` writes it), `None` exactly
    /// where the arch has no ISA-mode var.
    pub isa_mode_switch_vn: Option<Vn>,
    /// [`SleighArch::volatile_register_ranges`].
    pub volatile_ranges: Vec<Range<u64>>,
    /// [`SleighArch::internal_registers`] the sla declares.
    pub internal_register_vns: Vec<Vn>,
}

impl DecodeTables {
    /// # Errors
    ///
    /// When `sleigh`, which may be another arch's, and `arch` disagree: the
    /// sla does not flow the ISA-mode var, does not declare a var the arch
    /// re-derives, pairs `ISAModeSwitch` differently, or its `ram` space is
    /// missing, misnamed or not byte-addressed.
    pub(super) fn from_sleigh<R: MemReader>(
        arch: SleighArch,
        sleigh: &Sleigh<R>,
        regs: &SleighRegs,
    ) -> anyhow::Result<Self> {
        // Not `unwrap_or_default`: with an empty table the CFG classifies every
        // `CallOther` as returning, so a no-return one decodes on into whatever
        // follows it, and the lift rejects every one as an unknown user-op.
        let user_op_names = sleigh.user_op_names()?;
        let flow_vars = sleigh.flow_context_vars()?;
        // Every region re-imposes its mode through the flowing set; a mode var
        // the sla does not flow would make that a silent no-op.
        if let Some(var) = arch.isa_mode_var()
            && !flow_vars.iter().any(|n| n == var)
        {
            anyhow::bail!(
                "{arch:?}: ISA-mode var {var} is not among the sla's flowing context vars"
            );
        }
        // Read on a fresh engine, so these are the pspec defaults.  The address
        // is immaterial: a default is committed globally.
        let entry_defaults = flow_vars
            .iter()
            .map(|name| sleigh.get_context_at(0, name))
            .collect::<Result<_, _>>()?;
        // A hard error, unlike `transient_defaults` below: a named var the sla
        // does not declare means this preset's list is wrong, and silently
        // dropping it would leave the leak it exists to close.
        let flow_commit = arch
            .flow_commit_vars()
            .iter()
            .map(|&name| {
                let class = if flow_vars.iter().any(|n| n == name) {
                    FlowCommit::Flowing
                } else {
                    FlowCommit::NoFlow
                };
                Ok((name, class, sleigh.get_context_at(0, name)?))
            })
            .collect::<anyhow::Result<_>>()?;
        let transient_defaults = arch
            .transient_decode_vars()
            .iter()
            .filter_map(|name| Some((*name, sleigh.get_context_at(0, name).ok()?)))
            .collect();
        // `VnSpace::RAM` is the shortcut byte `b'r'`, which Sleigh derives from
        // the first character of a space's name, bumping on collision.  A
        // second processor space named with an `r` would take a bumped
        // shortcut and leave `RAM` naming whichever of the two was declared
        // first, silently retargeting every Load / Store the lift emits.
        let ram = sleigh.space_info(VnSpace::RAM).ok_or_else(|| {
            anyhow::anyhow!("{arch:?}: sleigh declares no address space with shortcut 'r'")
        })?;
        let ram_name = ram.name()?;
        anyhow::ensure!(
            ram_name == "ram",
            "{arch:?}: the address space with shortcut 'r' is named {ram_name:?}, not \"ram\""
        );
        // GHIDRA scales a LOAD/STORE offset by the space's word size
        // (`AddrSpace::addressToByte`, `emulate.cc:243`); the lift uses the
        // offset as a byte address directly, so a word-addressed space would
        // read the wrong bytes with no diagnostic.
        let ram_word_size = ram.word_size();
        anyhow::ensure!(
            ram_word_size == 1,
            "{arch:?}: \"ram\" has word size {ram_word_size}, and the lift indexes it in bytes"
        );
        // Capture and carry are paired BOTH ways: `name_to_vn` answers `None`
        // for a misspelling rather than erroring, so a one-directional check
        // would pass vacuously while the mode bit is never captured.
        let isa_mode_switch_vn = regs.name_to_vn("ISAModeSwitch");
        anyhow::ensure!(
            isa_mode_switch_vn.is_some() == arch.isa_mode_var().is_some(),
            "{arch:?}: ISAModeSwitch resolved={}, isa_mode_var={:?}",
            isa_mode_switch_vn.is_some(),
            arch.isa_mode_var(),
        );
        Ok(Self {
            user_op_names,
            flow_vars,
            entry_defaults,
            flow_commit,
            transient_defaults,
            declared_reg_vns: regs.vns().collect(),
            isa_mode_switch_vn,
            volatile_ranges: arch.volatile_register_ranges(regs)?,
            internal_register_vns: arch
                .internal_registers()
                .iter()
                .filter_map(|name| regs.name_to_vn(name))
                .collect(),
        })
    }
}

impl SleighArch {
    /// This arch's [`DecodeTables`], built once per preset per process.
    ///
    /// # Panics
    ///
    /// When the compiled-in sla does not load or disagrees with the preset's
    /// own tables: a preset edit gone wrong, which
    /// `every_preset_has_a_user_op_table_and_an_ram_named_ram` sweeps.
    #[must_use]
    pub fn decode_tables(self) -> &'static DecodeTables {
        &self.probe().1
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeTables, FlowCommit};
    use crate::SleighArch;

    fn mismatch(arch: SleighArch, sla_of: SleighArch) -> String {
        let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
        let sleigh = sla_of.sleigh(probe).expect("sleigh");
        let regs = sleigh.regs().expect("regs");
        DecodeTables::from_sleigh(arch, &sleigh, &regs)
            .expect_err("a mismatched pair")
            .to_string()
    }

    /// ARM paired with the x86-64 sla: the mode var it re-imposes at every
    /// region is not one that sla flows, so the re-impose would be a silent
    /// no-op.
    #[test]
    fn an_isa_mode_var_the_sla_does_not_flow_is_an_error() {
        let err = mismatch(SleighArch::arm(), SleighArch::x86_64());
        assert!(err.contains("TMode"), "{err}");
    }

    /// x86-64 paired with the ARM sla: the sla declares `ISAModeSwitch`, so a
    /// branch's mode commit would be captured for an arch with no mode to
    /// carry it into.
    #[test]
    fn an_unpaired_isa_mode_switch_is_an_error() {
        let err = mismatch(SleighArch::x86_64(), SleighArch::arm());
        assert!(err.contains("ISAModeSwitch"), "{err}");
    }

    /// What a re-derive erases a committed var to is the sla's own `noflow`
    /// declaration: ARM's IT-block `condit` is `noflow` and ThumbEE's `TEEMode`
    /// flows, on the big-endian sla as on the little-endian one.
    #[test]
    fn commit_class_is_the_slas_flow_declaration() {
        let want = [
            ("condit", FlowCommit::NoFlow),
            ("TEEMode", FlowCommit::Flowing),
        ];
        let mut checked = 0;
        for &preset in crate::ArchPreset::ALL {
            for &(name, class, _) in &preset.arch().decode_tables().flow_commit {
                let (_, expected) = want
                    .iter()
                    .find(|(n, _)| *n == name)
                    .unwrap_or_else(|| panic!("{preset:?}: no expected class for `{name}`"));
                assert_eq!(class, *expected, "{preset:?}: `{name}`");
                checked += 1;
            }
        }
        assert_eq!(
            checked, 8,
            "the four ARM32 presets re-derive condit and TEEMode"
        );
    }
}
