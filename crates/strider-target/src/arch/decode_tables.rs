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
    /// The declared register file.
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
    /// When `sleigh` and `arch` disagree: the ISA-mode var does not flow, a
    /// re-derived var is undeclared, `ISAModeSwitch` pairs differently, or the
    /// `ram` space is missing, misnamed or not byte-addressed.
    pub(super) fn from_sleigh<R: MemReader>(
        arch: SleighArch,
        sleigh: &Sleigh<R>,
        regs: &SleighRegs,
    ) -> anyhow::Result<Self> {
        let user_op_names = sleigh.user_op_names()?;
        let flow_vars = sleigh.flow_context_vars()?;
        // A region re-imposes its mode through the flowing set.
        if let Some(var) = arch.isa_mode_var()
            && !flow_vars.iter().any(|n| n == var)
        {
            anyhow::bail!(
                "{arch:?}: ISA-mode var {var} is not among the sla's flowing context vars"
            );
        }
        // A fresh engine: the pspec defaults, committed globally.
        let entry_defaults = flow_vars
            .iter()
            .map(|name| sleigh.get_context_at(0, name))
            .collect::<Result<_, _>>()?;
        // An undeclared var is an error here, unlike in `transient_defaults`.
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
        // Sleigh derives shortcut `r` from a space's name, bumping on
        // collision, so it need not be `ram`.
        let ram = sleigh.space_info(VnSpace::RAM).ok_or_else(|| {
            anyhow::anyhow!("{arch:?}: sleigh declares no address space with shortcut 'r'")
        })?;
        let ram_name = ram.name()?;
        anyhow::ensure!(
            ram_name == "ram",
            "{arch:?}: the address space with shortcut 'r' is named {ram_name:?}, not \"ram\""
        );
        // The lift uses a LOAD / STORE offset as a byte address.
        let ram_word_size = ram.word_size();
        anyhow::ensure!(
            ram_word_size == 1,
            "{arch:?}: \"ram\" has word size {ram_word_size}, and the lift indexes it in bytes"
        );
        // Both ways: `name_to_vn` answers a misspelling with `None`.
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
    /// When the compiled-in sla does not load or disagrees with the preset.
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
