#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Endianness {
    /// Least-significant byte at the lowest address (x86, AArch64 LE).
    #[default]
    Little,
    /// Most-significant byte at the lowest address (MIPS BE, AArch64 BE).
    Big,
}

impl Endianness {
    /// Decodes an N-byte word into a `u128`: `[0x01,0x02,0x03,0x04]` gives
    /// `0x0403_0201` little-endian, `0x0102_0304` big-endian.  This is the
    /// optimizer-side decode of the raw bytes `ReadOnlyMemory::read` fills.
    ///
    /// # Panics
    ///
    /// Panics if `bytes.len() > 16`.
    pub fn read_uint(self, bytes: &[u8]) -> u128 {
        let n = bytes.len();
        assert!(n <= 16, "read_uint supports at most 16 bytes, got {n}");
        let mut buf = [0u8; 16];
        match self {
            Self::Little => {
                buf[..n].copy_from_slice(bytes);
                u128::from_le_bytes(buf)
            }
            // BE bytes go in the high slots so the widened word still reads
            // as an N-byte big-endian value.
            Self::Big => {
                buf[16 - n..].copy_from_slice(bytes);
                u128::from_be_bytes(buf)
            }
        }
    }
}

#[cfg(test)]
mod endianness_tests {
    use super::Endianness;

    #[test]
    fn read_uint_decodes_n_byte_word_per_endianness() {
        let bytes = [0x01, 0x02, 0x03, 0x04];
        assert_eq!(Endianness::Little.read_uint(&bytes), 0x0403_0201);
        assert_eq!(Endianness::Big.read_uint(&bytes), 0x0102_0304);
    }

    #[test]
    fn read_uint_single_byte_is_endianness_invariant() {
        assert_eq!(Endianness::Little.read_uint(&[0xab]), 0xab);
        assert_eq!(Endianness::Big.read_uint(&[0xab]), 0xab);
    }

    #[test]
    fn read_uint_full_16_bytes_decodes_full_u128() {
        let bytes: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(
            Endianness::Little.read_uint(&bytes),
            u128::from_le_bytes(bytes),
        );
        assert_eq!(
            Endianness::Big.read_uint(&bytes),
            u128::from_be_bytes(bytes)
        );
        // The high byte participates; a u64-width decode would lose it.
        assert_eq!(
            Endianness::Big.read_uint(&bytes) >> 120,
            0x00,
            "BE high byte is first source byte",
        );
        assert_eq!(
            Endianness::Little.read_uint(&bytes) >> 120,
            0xff,
            "LE high byte is last source byte",
        );
    }

    #[test]
    fn read_uint_full_u64_matches_native_u64_decode() {
        let bytes = [0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89];
        assert_eq!(
            Endianness::Little.read_uint(&bytes),
            u128::from(u64::from_le_bytes(bytes)),
        );
        assert_eq!(
            Endianness::Big.read_uint(&bytes),
            u128::from(u64::from_be_bytes(bytes)),
        );
    }
}

/// Dispatch key for [`crate::call_other_abi::classify`].  One variant per
/// [`SleighArch`] preset, deliberately finer-grained than the SLA spec: the
/// three 32-bit ARM variants share `ARM8` but could diverge on CallOther
/// semantics.  Presets that agree today share a match arm via `|`
/// alternation in `classify_arch_specific`; a divergence splits the arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchPreset {
    X86,
    X86_64,
    Arm,
    ArmBe,
    /// BE8 ARM (little-endian instructions, big-endian data), i.e. modern
    /// ARMv6+ big-endian.  Same instruction set and CallOther ABI as
    /// [`ArchPreset::Arm`]; only the Sleigh spec and data endianness differ.
    ArmBeKernel,
    ArmThumb,
    Aarch64,
    Aarch64Be,
    MipsBe32,
    MipsLe32,
    MipsBe64,
    MipsLe64,
    Ppc32Be,
    Ppc32Le,
    Ppc64Be,
    Ppc64Le,
}

/// The Sleigh configuration describing one target architecture.
///
/// The stack-pointer register name lives on the calling convention, not
/// here, so `CallingConvention::build` is self-contained and two ABIs on
/// one arch can in principle declare different SP registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    sla_spec: rsleigh::sla_spec::SlaSpec,
    pspec: rsleigh::pspec::PSpec,
    endianness: Endianness,
    pub(crate) preset: ArchPreset,
}

/// Emits a `pub fn $name() -> SleighArch` preset constructor, threading the
/// caller's rustdoc through as `$(#[$doc])*`.
macro_rules! arch_ctor {
    ($(#[$doc:meta])* $name:ident => $sla:ident, $pspec:ident, $endian:ident, $preset:ident) => {
        $(#[$doc])*
        pub fn $name() -> SleighArch {
            SleighArch {
                sla_spec: rsleigh::sla_spec::$sla,
                pspec: rsleigh::pspec::$pspec,
                endianness: Endianness::$endian,
                preset: ArchPreset::$preset,
            }
        }
    };
}

impl SleighArch {
    pub fn sla_spec(&self) -> rsleigh::sla_spec::SlaSpec {
        self.sla_spec
    }
    pub fn pspec(&self) -> rsleigh::pspec::PSpec {
        self.pspec
    }
    pub fn endianness(&self) -> Endianness {
        self.endianness
    }

    /// Set by each preset constructor; not user-overridable.
    pub fn preset(&self) -> ArchPreset {
        self.preset
    }

    arch_ctor! {
        x86_64 => SLA_SPEC_X86_64, PSPEC_X86_64, Little, X86_64
    }

    arch_ctor! {
        x86 => SLA_SPEC_X86, PSPEC_X86, Little, X86
    }

    arch_ctor! {
        mipsbe32 => SLA_SPEC_MIPS32BE, PSPEC_MIPS32, Big, MipsBe32
    }

    arch_ctor! {
        mipsle32 => SLA_SPEC_MIPS32LE, PSPEC_MIPS32, Little, MipsLe32
    }

    arch_ctor! {
        /// ARM 32-bit little-endian, non-Thumb.  `ARM8_le` + `ARM_v45` matches
        /// the `-marm` target in `fixtures/arch/arm.mk`.
        arm => SLA_SPEC_ARM8_LE, PSPEC_ARM_V45, Little, Arm
    }

    arch_ctor! {
        aarch64 => SLA_SPEC_AARCH64, PSPEC_AARCH64, Little, Aarch64
    }

    arch_ctor! {
        aarch64be => SLA_SPEC_AARCH64BE, PSPEC_AARCH64, Big, Aarch64Be
    }

    arch_ctor! {
        /// Linux N64 ABI (`mips64-linux-gnuabi64-gcc`).
        mipsbe64 => SLA_SPEC_MIPS64BE, PSPEC_MIPS64, Big, MipsBe64
    }

    arch_ctor! {
        /// Linux N64 ABI (`mips64el-linux-gnuabi64-gcc`).
        mipsle64 => SLA_SPEC_MIPS64LE, PSPEC_MIPS64, Little, MipsLe64
    }

    arch_ctor! {
        /// `powerpc-linux-gnu-gcc`, System V 32-bit ABI.
        ppc32be => SLA_SPEC_PPC_32_BE, PSPEC_PPC_32, Big, Ppc32Be
    }

    arch_ctor! {
        /// `powerpc-linux-gnu-gcc -mlittle-endian`.  Uncommon as a Linux
        /// target, but the Sleigh spec exists and mirrors `ppc32be`.
        ppc32le => SLA_SPEC_PPC_32_LE, PSPEC_PPC_32, Little, Ppc32Le
    }

    arch_ctor! {
        /// `powerpc64-linux-gnu-gcc`, ELFv1 ABI with function descriptors.
        /// Needs the Power ISA + Altivec sla spec: the stripped `PPC_64_BE`
        /// spec rejects Power7+ scalar ops (`popcntw`, `popcntd`, `cntlzd`,
        /// `cnttzd`) and Altivec vector ops with `Unable to resolve
        /// constructor`.
        ppc64be => SLA_SPEC_PPC_64_ISA_ALTIVEC_BE, PSPEC_PPC_64, Big, Ppc64Be
    }

    arch_ctor! {
        /// `powerpc64le-linux-gnu-gcc`, ELFv2 ABI: no function descriptors,
        /// dot-prefixed symbols.  Power ISA + Altivec sla spec, see `ppc64be`.
        ppc64le => SLA_SPEC_PPC_64_ISA_ALTIVEC_LE, PSPEC_PPC_64, Little, Ppc64Le
    }

    arch_ctor! {
        /// ARM Cortex-M, Thumb-2 only (`arm-linux-gnueabihf-gcc -mthumb`).
        /// `ARM8_le` decodes both ARM and Thumb; the `ARMCORTEX` pspec is
        /// what selects Thumb-only Cortex-M decoding.
        arm_thumb => SLA_SPEC_ARM8_LE, PSPEC_ARMCORTEX, Little, ArmThumb
    }

    arch_ctor! {
        /// Legacy **BE32** ARM 32-bit: big-endian instructions AND data,
        /// pre-ARMv6.  ARM AAPCS is byte-order independent, so `arm_aapcs`
        /// pairs with both this and [`SleighArch::arm`].
        ///
        /// Modern ARMv6+ big-endian Linux is **BE8** (little-endian
        /// instructions, big-endian data, flagged `EF_ARM_BE8`), not BE32.
        /// This spec byte-reverses every instruction word and fails almost
        /// every decode on such a binary; use [`SleighArch::arm_be_kernel`].
        ///
        /// Built with `clang --target=armeb-linux-gnueabi` linked by the
        /// `arm-linux-gnueabihf` GNU `ld`: lld 14 has no
        /// `armelfb_linux_eabi` emulation, GNU `ld` handles `-EB` via the BE
        /// BFD target.
        arm_be => SLA_SPEC_ARM8_BE, PSPEC_ARM_V45, Big, ArmBe
    }

    arch_ctor! {
        /// **BE8** ARM 32-bit (GHIDRA's `ARM:LEBE:32`): little-endian
        /// instructions, big-endian data.  Every ARMv6+ big-endian Linux
        /// target, kernel and userland, is this.
        ///
        /// The LE `ARM8_le` spec decodes the instruction words; the paired
        /// `Endianness::Big` drives strider's own data decoding
        /// (`LoadReadOnly` ROM reads, jump-table entries).  Pairs with the
        /// byte-order-independent `arm_aapcs` CC.  Pass explicitly:
        /// `load_elf(vmlinux, arch=SleighArch.arm_be_kernel())`.
        arm_be_kernel => SLA_SPEC_ARM8_LE, PSPEC_ARM_V45, Big, ArmBeKernel
    }

    /// Extracts this arch's register table by probing Sleigh against an
    /// empty memory reader, for callers that need a `SleighRegs` without
    /// decoding any code.
    ///
    /// # Errors
    ///
    /// Propagates errors from `rsleigh::Sleigh::new` or `Sleigh::regs`.
    pub fn probe_regs(self) -> anyhow::Result<rsleigh::SleighRegs> {
        let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
        let sleigh = rsleigh::Sleigh::new(self.sla_spec, self.pspec, probe)?;
        Ok(sleigh.regs()?)
    }
}
