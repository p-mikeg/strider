mod decode_tables;
mod endian;
mod names;
#[cfg(test)]
mod tests;

pub use decode_tables::{DecodeTables, FlowCommit};
pub use endian::{ByteOrder, Endianness};
pub use names::ArchNames;

use crate::family::{self, DecodeFacts, Family};

/// Emits `ArchPreset`, [`ArchPreset::ALL`] and every per-preset fact from one
/// row list; each row's doc lands on the variant and its constructor.
macro_rules! arch_presets {
    ($(
        $(#[$doc:meta])*
        $variant:ident => $name:literal, $ctor:ident, $family:ident,
                          $sla:ident, $pspec:ident, $endian:ident,
                          $register_endian:ident, $ghidra_id:literal;
    )+) => {
        /// One variant per [`SleighArch`] preset.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ArchPreset {
            $($(#[$doc])* $variant,)+
        }

        impl ArchPreset {
            /// Every variant.
            pub const ALL: &'static [ArchPreset] = &[$(ArchPreset::$variant,)+];

            /// The [`SleighArch`] constructor's name, e.g. `"x86_64"` or
            /// `"arm_be_kernel"`.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name,)+ }
            }

            #[must_use]
            pub(crate) const fn family(self) -> &'static Family {
                match self { $(Self::$variant => &family::$family,)+ }
            }

            /// The family's name: `"x86"`, `"arm32"`, `"aarch64"`, `"mips"` or
            /// `"ppc"`.
            #[cfg(any(test, feature = "test-util"))]
            #[must_use]
            pub const fn family_name(self) -> &'static str {
                self.family().name
            }

            /// The Ghidra `.ldefs` file this preset is a row of, e.g.
            /// `"x86/data/languages/x86.ldefs"`.
            #[cfg(any(test, feature = "test-util"))]
            #[must_use]
            pub const fn ghidra_ldefs(self) -> &'static str {
                self.family().ghidra_ldefs
            }

            /// The Ghidra language id this preset is.
            #[cfg(any(test, feature = "test-util"))]
            #[must_use]
            pub const fn ghidra_id(self) -> &'static str {
                match self { $(Self::$variant => $ghidra_id,)+ }
            }

            /// The byte order of the Sleigh REGISTER space,
            /// [`SleighArch::register_endianness`].
            const fn register_endianness(self) -> Endianness {
                match self { $(Self::$variant => Endianness::$register_endian,)+ }
            }

            /// The arch this preset names, inverse of [`SleighArch::preset`].
            #[cfg(any(test, feature = "test-util"))]
            #[must_use]
            pub fn arch(self) -> SleighArch {
                match self { $(Self::$variant => SleighArch::$ctor(),)+ }
            }
        }

        impl SleighArch {
            $(
                $(#[$doc])*
                pub fn $ctor() -> SleighArch {
                    static SLA: rsleigh::sla_spec::SlaSpec = rsleigh::sla_spec::$sla;
                    static PSPEC: rsleigh::pspec::PSpec = rsleigh::pspec::$pspec;
                    SleighArch {
                        sla_spec: SLA,
                        pspec: PSPEC,
                        endianness: Endianness::$endian,
                        preset: ArchPreset::$variant,
                    }
                }
            )+
        }
    };
}

arch_presets! {
    X86 => "x86", x86, X86, SLA_SPEC_X86, PSPEC_X86, Little, Little, "x86:LE:32:default";
    X86_64 => "x86_64", x86_64, X86, SLA_SPEC_X86_64, PSPEC_X86_64, Little, Little, "x86:LE:64:default";
    /// ARM 32-bit little-endian, non-Thumb.
    Arm => "arm", arm, ARM32, SLA_SPEC_ARM8_LE, PSPEC_ARMT, Little, Little, "ARM:LE:32:v8";
    /// Legacy pre-ARMv6 **BE32** ARM: big-endian instructions and data. For
    /// ARMv6+ big-endian Linux (BE8) use [`SleighArch::arm_be_kernel`]. NEON
    /// lane values are meaningless here: the sla disagrees with itself on order.
    ArmBe => "arm_be", arm_be, ARM32, SLA_SPEC_ARM8_BE, PSPEC_ARMT, Big, Big, "ARM:BE:32:v8";
    /// **BE8** ARM 32-bit (Ghidra's `ARM:LEBE:32`): little-endian
    /// instructions, big-endian data; every ARMv6+ big-endian Linux target.
    ArmBeKernel => "arm_be_kernel", arm_be_kernel, ARM32, SLA_SPEC_ARM8_BE8, PSPEC_ARMT, Big, Little, "ARM:LEBE:32:v8LEInstruction";
    /// ARM 32-bit `-mthumb`: Cortex-M pspec defaults over the full-ARM-state
    /// sla, so ARM state stays reachable (`arm-linux-gnueabihf-gcc -mthumb`).
    ArmThumb => "arm_thumb", arm_thumb, ARM32, SLA_SPEC_ARM8_LE, PSPEC_ARMCORTEX, Little, Little, "ARM:LE:32:v8T";
    Aarch64 => "aarch64", aarch64, AARCH64, SLA_SPEC_AARCH64, PSPEC_AARCH64, Little, Little, "AARCH64:LE:64:v8A";
    Aarch64Be => "aarch64be", aarch64be, AARCH64, SLA_SPEC_AARCH64BE, PSPEC_AARCH64, Big, Big, "AARCH64:BE:64:v8A";
    MipsBe32 => "mipsbe32", mipsbe32, MIPS, SLA_SPEC_MIPS32BE, PSPEC_MIPS32, Big, Big, "MIPS:BE:32:default";
    MipsLe32 => "mipsle32", mipsle32, MIPS, SLA_SPEC_MIPS32LE, PSPEC_MIPS32, Little, Little, "MIPS:LE:32:default";
    /// Linux N64 ABI (`mips64-linux-gnuabi64-gcc`).
    MipsBe64 => "mipsbe64", mipsbe64, MIPS, SLA_SPEC_MIPS64BE, PSPEC_MIPS64, Big, Big, "MIPS:BE:64:default";
    /// Linux N64 ABI (`mips64el-linux-gnuabi64-gcc`).
    MipsLe64 => "mipsle64", mipsle64, MIPS, SLA_SPEC_MIPS64LE, PSPEC_MIPS64, Little, Little, "MIPS:LE:64:default";
    /// `powerpc-linux-gnu-gcc`, System V 32-bit ABI.
    Ppc32Be => "ppc32be", ppc32be, PPC, SLA_SPEC_PPC_32_BE, PSPEC_PPC_32, Big, Big, "PowerPC:BE:32:default";
    /// `powerpc-linux-gnu-gcc -mlittle-endian`.
    Ppc32Le => "ppc32le", ppc32le, PPC, SLA_SPEC_PPC_32_LE, PSPEC_PPC_32, Little, Little, "PowerPC:LE:32:default";
    /// `powerpc64-linux-gnu-gcc`, ELFv1 ABI with function descriptors; the
    /// Power ISA + Altivec sla.
    Ppc64Be => "ppc64be", ppc64be, PPC, SLA_SPEC_PPC_64_ISA_ALTIVEC_BE, PSPEC_PPC_64, Big, Big, "PowerPC:BE:64:A2ALT";
    /// `powerpc64le-linux-gnu-gcc`, ELFv2 ABI; the Power ISA + Altivec sla.
    Ppc64Le => "ppc64le", ppc64le, PPC, SLA_SPEC_PPC_64_ISA_ALTIVEC_LE, PSPEC_PPC_64, Little, Little, "PowerPC:LE:64:A2ALT";
}

/// The Sleigh configuration describing one target architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    sla_spec: rsleigh::sla_spec::SlaSpec,
    pspec: rsleigh::pspec::PSpec,
    endianness: Endianness,
    pub(crate) preset: ArchPreset,
}

impl SleighArch {
    /// A Sleigh over `reader` from this preset's sla and pspec.
    ///
    /// # Errors
    /// When the compiled-in sla does not load.
    pub fn sleigh<R: rsleigh::MemReader>(&self, reader: R) -> anyhow::Result<rsleigh::Sleigh<R>> {
        rsleigh::Sleigh::new(self.sla_spec, self.pspec, reader)
            .map_err(|e| anyhow::anyhow!("{:?}: Sleigh::new failed: {e}", self.preset))
    }

    pub fn sla_spec(&self) -> rsleigh::sla_spec::SlaSpec {
        self.sla_spec
    }
    pub fn pspec(&self) -> rsleigh::pspec::PSpec {
        self.pspec
    }
    pub fn endianness(&self) -> Endianness {
        self.endianness
    }

    /// The byte order of the Sleigh REGISTER space (the sla's `ENDIAN`), which
    /// sub-register slicing follows. `arm_be_kernel` is a little-endian sla
    /// with big-endian data.
    pub fn register_endianness(&self) -> Endianness {
        self.preset.register_endianness()
    }

    /// [`Self::endianness`] and [`Self::register_endianness`] together.
    pub fn byte_order(&self) -> ByteOrder {
        ByteOrder {
            data: self.endianness(),
            register: self.register_endianness(),
        }
    }

    pub fn preset(&self) -> ArchPreset {
        self.preset
    }

    fn facts(&self) -> &'static DecodeFacts {
        &self.preset.family().facts
    }

    /// The context variable holding the address-low-bit ISA mode (ARM32 and
    /// MIPS), or `None`.
    #[must_use]
    pub fn isa_mode_var(&self) -> Option<&'static str> {
        self.facts().isa_mode_var
    }

    /// Context vars a constructor commits at a later address, which a CFG
    /// build re-derives by re-lifting the committing instruction. Only ARM
    /// needs any; PowerPC `linkreg` is committed but cannot change the lift.
    #[must_use]
    pub fn flow_commit_vars(&self) -> &'static [&'static str] {
        self.facts().flow_commit_vars
    }

    /// Whether the sla decodes a branch together with its delay slot, one
    /// `lift_one` spanning both.
    #[must_use]
    pub fn has_delay_slots(&self) -> bool {
        self.facts().has_delay_slots
    }

    /// `noflow` context vars that change which constructor matches: reset to
    /// the pspec default before each decode, then the previous instruction is
    /// re-lifted so its own commit lands.
    #[must_use]
    pub fn transient_decode_vars(&self) -> &'static [&'static str] {
        self.facts().transient_decode_vars
    }

    /// Whether a `CallOther` writing a sub-register of `container` leaves the
    /// container's other bytes intact. True on x86, whose sla zero-extends
    /// every VEX destination into its ZMM register, except for the 10-byte
    /// x87 `ST` registers, whose MMX write redefines sign and exponent.
    #[must_use]
    pub fn callother_output_is_exact_in(&self, container: &rsleigh::Vn) -> bool {
        self.facts().callother_output_exact && container.size != 10
    }

    /// The REGISTER-space bytes a register-space LOAD or STORE can address,
    /// or `None` for the whole space. An access outside it is assumed not to
    /// exist.
    #[must_use]
    pub fn register_space_reach(&self) -> Option<core::ops::Range<u64>> {
        self.facts().register_space_reach.clone()
    }

    /// Registers the sla declares for its own semantics rather than as
    /// processor state (per-instruction scratch, fake storage such as
    /// PowerPC `r2Save`), which no callee can read.
    #[must_use]
    pub fn internal_registers(&self) -> &'static [&'static str] {
        self.facts().internal_registers
    }

    /// REGISTER-space byte ranges whose value changes with nothing writing
    /// it: the pspec's volatile ranges and registers plus the family's
    /// additions, less its exclusions.
    ///
    /// # Errors
    /// A volatile range outside the REGISTER space, or an undeclared name.
    pub fn volatile_register_ranges(
        &self,
        regs: &rsleigh::SleighRegs,
    ) -> crate::Result<Vec<core::ops::Range<u64>>> {
        let pspec = self.pspec();
        let mut ranges = Vec::new();
        for range in pspec.volatile_ranges {
            anyhow::ensure!(
                range.space == "register",
                "{:?}: a volatile range in space {:?}",
                self.preset,
                range.space
            );
            ranges.push(range.first..range.last.saturating_add(1));
        }
        let names = pspec
            .volatile_registers
            .iter()
            .chain(self.facts().volatile_registers);
        for vn in crate::calling_convention::regs_to_vns(regs, names)? {
            ranges.push(vn.addr_off..vn.addr_off + u64::from(vn.size));
        }
        let excluded =
            crate::calling_convention::regs_to_vns(regs, self.facts().non_volatile_registers)?;
        for vn in excluded {
            let (lo, hi) = (vn.addr_off, vn.addr_off + u64::from(vn.size));
            ranges = ranges
                .into_iter()
                .flat_map(|r| [r.start..r.end.min(lo), r.start.max(hi)..r.end])
                .filter(|r| !r.is_empty())
                .collect();
        }
        Ok(ranges)
    }

    /// The `(context_var, value)` a cold entry at `entry_addr` decodes in:
    /// the address low bit selects the alternate ISA (Thumb, MIPS16e).
    pub fn entry_mode_context(&self, entry_addr: u64) -> Option<(&'static str, u32)> {
        self.isa_mode_var()
            .map(|var| (var, u32::from(entry_addr & 1 == 1)))
    }

    /// [`Self::names`] and [`Self::decode_tables`], read off one Sleigh over
    /// an empty image once per preset per process.
    fn probe(self) -> &'static (ArchNames, DecodeTables) {
        static PROBE: [std::sync::OnceLock<(ArchNames, DecodeTables)>; ArchPreset::ALL.len()] =
            [const { std::sync::OnceLock::new() }; ArchPreset::ALL.len()];
        PROBE[self.preset() as usize].get_or_init(|| {
            let empty = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
            let sleigh = self
                .sleigh(empty)
                .unwrap_or_else(|e| panic!("the compiled-in sla does not load: {e:#}"));
            let names = ArchNames::from_sleigh(&sleigh)
                .unwrap_or_else(|e| panic!("{:?}: {e:#}", self.preset()));
            let tables = DecodeTables::from_sleigh(self, &sleigh, names.regs())
                .unwrap_or_else(|e| panic!("{e:#}"));
            (names, tables)
        })
    }

    /// This arch's register table, a copy of [`SleighArch::names`]'s.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn probe_regs(self) -> rsleigh::SleighRegs {
        self.names().regs().clone()
    }
}
