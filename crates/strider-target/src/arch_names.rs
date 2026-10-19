//! The names an arch's sla gives its registers and address spaces: all a
//! render reads, so a render needs no decoder.

use std::fmt;
use std::sync::OnceLock;

use rsleigh::{MemReader, Sleigh, SleighRegs, Vn, VnSpace};

use crate::arch::{ArchPreset, SleighArch};

/// One arch's register and space names, its default code space and the byte
/// width of its `ram` addresses.
#[derive(Debug)]
pub struct ArchNames {
    regs: SleighRegs,
    /// Every space the sla defines, ascending by shortcut.
    spaces: Vec<(VnSpace, Box<str>)>,
    default_code_space: VnSpace,
    ram_addr_size: u32,
}

impl ArchNames {
    /// The tables `sleigh` was built from; every engine of one arch reads the
    /// same.
    ///
    /// # Errors
    ///
    /// The register table cannot be read, a space name is not UTF-8, or the
    /// sla declares no space with shortcut `r`.
    pub fn from_sleigh<R: MemReader>(sleigh: &Sleigh<R>) -> anyhow::Result<Self> {
        let regs = sleigh.regs()?;
        let spaces = (0..=u8::MAX)
            .filter_map(|shortcut| {
                let space = VnSpace::new(shortcut);
                let info = sleigh.space_info(space)?;
                Some(info.name().map(|name| (space, Box::<str>::from(name))))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ram_addr_size = sleigh
            .space_info(VnSpace::RAM)
            .ok_or_else(|| anyhow::anyhow!("the sla declares no space with shortcut 'r'"))?
            .addr_size();
        Ok(Self {
            regs,
            spaces,
            default_code_space: sleigh.default_code_space(),
            ram_addr_size,
        })
    }

    #[must_use]
    pub fn regs(&self) -> &SleighRegs {
        &self.regs
    }

    #[must_use]
    pub fn default_code_space(&self) -> VnSpace {
        self.default_code_space
    }

    /// Bytes per `ram` address: the width a calling convention's stack
    /// geometry has to match.
    #[must_use]
    pub fn ram_addr_size(&self) -> u32 {
        self.ram_addr_size
    }

    /// `space` as rsleigh's `VnSpace::ctx_fmt` spells it: its sla name, or its
    /// shortcut character where the sla defines no such space.
    #[must_use]
    pub fn space(&self, space: VnSpace) -> impl fmt::Display + '_ {
        SpaceName { names: self, space }
    }

    /// `vn` as rsleigh's `Vn::ctx_fmt` spells it: a register's name, else
    /// `<space>[0x<off>]:<size>`, or `0x<off>:<size>` for a constant.
    #[must_use]
    pub fn vn(&self, vn: Vn) -> impl fmt::Display + '_ {
        VnName { names: self, vn }
    }

    fn space_name(&self, space: VnSpace) -> Option<&str> {
        self.spaces
            .binary_search_by_key(&space.shortcut_raw(), |(s, _)| s.shortcut_raw())
            .ok()
            .map(|i| &*self.spaces[i].1)
    }
}

struct SpaceName<'a> {
    names: &'a ArchNames,
    space: VnSpace,
}

impl fmt::Display for SpaceName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.names.space_name(self.space) {
            Some(name) => f.write_str(name),
            None => write!(f, "{}", self.space),
        }
    }
}

struct VnName<'a> {
    names: &'a ArchNames,
    vn: Vn,
}

impl fmt::Display for VnName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.names.regs.vn_to_name(self.vn) {
            return f.write_str(name);
        }
        let Vn {
            addr_space,
            addr_off,
            size,
        } = self.vn;
        if addr_space == VnSpace::CONST {
            write!(f, "0x{addr_off:x}:{size}")
        } else {
            write!(f, "{}[0x{addr_off:x}]:{size}", self.names.space(addr_space))
        }
    }
}

impl SleighArch {
    /// This arch's [`ArchNames`], built once per preset per process from a
    /// Sleigh over an empty image.
    #[must_use]
    pub fn names(self) -> &'static ArchNames {
        static NAMES: [OnceLock<ArchNames>; ArchPreset::ALL.len()] =
            [const { OnceLock::new() }; ArchPreset::ALL.len()];
        NAMES[self.preset() as usize].get_or_init(|| {
            let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
            let sleigh = Sleigh::new(self.sla_spec(), self.pspec(), probe).unwrap_or_else(|e| {
                panic!(
                    "{:?}: the compiled-in sla does not load: {e}",
                    self.preset()
                )
            });
            ArchNames::from_sleigh(&sleigh).unwrap_or_else(|e| panic!("{:?}: {e:#}", self.preset()))
        })
    }
}

#[cfg(test)]
mod tests {
    use rsleigh::{Vn, VnSpace};

    use crate::ArchPreset;

    /// The table renders exactly what the decoding engine's `ctx_fmt` does,
    /// for every register, for a non-register varnode in every space
    /// shortcut (defined or not, CONST included), and for every space.
    #[test]
    fn names_render_what_the_decoding_engine_renders_on_every_preset() {
        for &preset in ArchPreset::ALL {
            let arch = preset.arch();
            let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
            let sleigh =
                rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), probe).expect("sleigh");
            let regs = sleigh.regs().expect("regs");
            let names = arch.names();
            assert_eq!(names.regs(), &regs, "{preset:?}");
            assert_eq!(
                names.default_code_space(),
                sleigh.default_code_space(),
                "{preset:?}"
            );
            assert_eq!(
                names.ram_addr_size(),
                sleigh.space_info(VnSpace::RAM).expect("ram").addr_size(),
                "{preset:?}"
            );
            for reg in regs.iter() {
                assert_eq!(
                    names.vn(reg.vn).to_string(),
                    reg.vn.ctx_fmt(&sleigh, &regs).to_string(),
                    "{preset:?} {}",
                    reg.name
                );
            }
            for shortcut in 0..=u8::MAX {
                let space = VnSpace::new(shortcut);
                assert_eq!(
                    names.space(space).to_string(),
                    space.ctx_fmt(&sleigh).to_string(),
                    "{preset:?} space {shortcut:#x}"
                );
                let vn = Vn {
                    addr_space: space,
                    addr_off: 0x1234_5678_9abc,
                    size: 3,
                };
                assert_eq!(
                    names.vn(vn).to_string(),
                    vn.ctx_fmt(&sleigh, &regs).to_string(),
                    "{preset:?} varnode in space {shortcut:#x}"
                );
            }
        }
    }

    #[test]
    fn a_preset_builds_its_table_once() {
        let arch = crate::SleighArch::arm_thumb();
        assert!(std::ptr::eq(arch.names(), arch.names()));
    }

    #[test]
    fn every_preset_has_its_own_name() {
        let names: std::collections::BTreeSet<&str> =
            ArchPreset::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(names.len(), ArchPreset::ALL.len());
        assert_eq!(
            ArchPreset::ALL.iter().map(|p| p.name()).collect::<Vec<_>>(),
            [
                "x86",
                "x86_64",
                "arm",
                "arm_be",
                "arm_be_kernel",
                "arm_thumb",
                "aarch64",
                "aarch64be",
                "mipsbe32",
                "mipsle32",
                "mipsbe64",
                "mipsle64",
                "ppc32be",
                "ppc32le",
                "ppc64be",
                "ppc64le",
            ]
        );
    }
}
