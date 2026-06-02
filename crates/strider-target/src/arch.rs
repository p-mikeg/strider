/// The byte order used by an architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Endianness {
    /// Least-significant byte at the lowest address (x86, AArch64 LE, …).
    #[default]
    Little,
    /// Most-significant byte at the lowest address (MIPS BE, AArch64 BE, …).
    Big,
}

impl Endianness {
    /// Decodes a 64-bit unsigned integer from `bytes` according to this
    /// byte order.  This is the single source of truth for the
    /// `if le { from_le_bytes } else { from_be_bytes }` branch, used by
    /// the `ReadOnlyMemory` read path in both
    /// `strider_reader::elf::ElfFileMemReader` and
    /// `strider-py`'s `PyMemoryMapReader` (each positions the N-byte
    /// payload in an 8-byte buffer at the endianness-appropriate end,
    /// then delegates the final decode here).  Callers that need to read
    /// a sub-`u64` word follow that buffer-positioning convention.
    #[must_use]
    pub fn read_u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Self::Little => u64::from_le_bytes(bytes),
            Self::Big => u64::from_be_bytes(bytes),
        }
    }

    /// Decodes the raw `bytes` (an N-byte little/big-endian word, with
    /// `N == bytes.len() <= 16`) into a `u128` according to this byte
    /// order.  This is the optimizer-side decode of the raw bytes a
    /// `ReadOnlyMemory::read` fills: e.g. `[0x01,0x02,0x03,0x04]` decodes
    /// to `0x0403_0201` little-endian, `0x0102_0304` big-endian.
    ///
    /// Bytes are placed into the endianness-appropriate end of a 16-byte
    /// buffer so the widened word reads as an N-byte value of this byte
    /// order.
    ///
    /// # Panics
    ///
    /// Panics if `bytes.len() > 16`.
    #[must_use]
    pub fn read_uint(self, bytes: &[u8]) -> u128 {
        let n = bytes.len();
        assert!(n <= 16, "read_uint supports at most 16 bytes, got {n}");
        let mut buf = [0u8; 16];
        match self {
            // LE: bytes occupy the low slots.
            Self::Little => {
                buf[..n].copy_from_slice(bytes);
                u128::from_le_bytes(buf)
            }
            // BE: bytes occupy the high slots so the widened word reads
            // as a big-endian N-byte value.
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
    fn read_u64_little_endian_matches_from_le_bytes() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(Endianness::Little.read_u64(bytes), u64::from_le_bytes(bytes));
    }

    #[test]
    fn read_u64_big_endian_matches_from_be_bytes() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(Endianness::Big.read_u64(bytes), u64::from_be_bytes(bytes));
    }

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
        // A full 16-byte word must decode to the complete u128 (no
        // truncation to the low 8 bytes).
        let bytes: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert_eq!(
            Endianness::Little.read_uint(&bytes),
            u128::from_le_bytes(bytes),
        );
        assert_eq!(Endianness::Big.read_uint(&bytes), u128::from_be_bytes(bytes));
        // Sanity: the high byte actually participates (would be lost on a
        // u64-width decode).
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
    fn read_uint_full_u64_matches_read_u64() {
        let bytes = [0x78, 0x56, 0x34, 0x12, 0xef, 0xcd, 0xab, 0x89];
        assert_eq!(
            Endianness::Little.read_uint(&bytes),
            u128::from(Endianness::Little.read_u64(bytes)),
        );
        assert_eq!(
            Endianness::Big.read_uint(&bytes),
            u128::from(Endianness::Big.read_u64(bytes)),
        );
    }
}

/// Architecture-preset discriminator used as a key for
/// [`crate::call_other_abi::classify`].  One variant per [`SleighArch`]
/// preset constructor — this gives full granularity, so Arm-32 vs
/// Arm-32 big-endian vs Arm Thumb-mode (which use the same `ARM8` SLA
/// spec but different `pspec`s and could in principle have different
/// CallOther semantics) are distinguishable.
///
/// In `call_other_abi::classify_arch_specific`, sets of presets that
/// share semantics use `|` alternation in the match: e.g. all three
/// 32-bit ARM variants share the Linux SVC/SWI ABI, so they alternate
/// in one arm.  When a future divergence appears (e.g. a Thumb-only
/// pcodeop), the alternation splits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchPreset {
    X86,
    X86_64,
    Arm,
    ArmBe,
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

/// A collection of Sleigh configuration items that together describe a
/// specific target architecture.
///
/// Pass a `SleighArch` to `Strider::new` (in the `strider` crate) along
/// with the calling convention to build a strider for that target.  The
/// calling convention owns the stack-pointer register name (see
/// [`crate::CallingConvention::build`]) rather than the arch, so that
/// `CallingConvention::build` is self-contained and different ABIs on the
/// same arch can in principle declare different SP registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    pub(crate) sla_spec: rsleigh::sla_spec::SlaSpec,
    pub(crate) pspec: rsleigh::pspec::PSpec,
    pub(crate) endianness: Endianness,
    pub(crate) preset: ArchPreset,
}

impl SleighArch {
    /// Read the `.sla` specification for the architecture's instruction set.
    #[must_use]
    pub fn sla_spec(&self) -> rsleigh::sla_spec::SlaSpec {
        self.sla_spec
    }
    /// Read the `.pspec` processor specification (register and space definitions).
    #[must_use]
    pub fn pspec(&self) -> rsleigh::pspec::PSpec {
        self.pspec
    }
    /// Read the byte order of this architecture.
    #[must_use]
    pub fn endianness(&self) -> Endianness {
        self.endianness
    }

    /// Read the arch-preset discriminator — used by
    /// [`crate::call_other_abi::classify`] to dispatch arch-specific user-op
    /// ABIs.  Set by each preset constructor; not user-overridable.
    #[must_use]
    pub fn preset(&self) -> ArchPreset {
        self.preset
    }

    /// Returns the x86-64 (64-bit Intel/AMD) architecture descriptor.
    #[must_use]
    pub fn x86_64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86_64,
            pspec: rsleigh::pspec::PSPEC_X86_64,
            endianness: Endianness::Little,
            preset: ArchPreset::X86_64,
        }
    }

    /// Returns the x86 (32-bit Intel/AMD) architecture descriptor.
    #[must_use]
    pub fn x86() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86,
            pspec: rsleigh::pspec::PSPEC_X86,
            endianness: Endianness::Little,
            preset: ArchPreset::X86,
        }
    }

    /// Returns the big-endian MIPS-32 architecture descriptor.
    #[must_use]
    pub fn mipsbe32() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32BE,
            pspec: rsleigh::pspec::PSPEC_MIPS32,
            endianness: Endianness::Big,
            preset: ArchPreset::MipsBe32,
        }
    }

    /// Returns the little-endian MIPS-32 architecture descriptor.
    #[must_use]
    pub fn mipsle32() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32LE,
            pspec: rsleigh::pspec::PSPEC_MIPS32,
            endianness: Endianness::Little,
            preset: ArchPreset::MipsLe32,
        }
    }

    /// Returns the little-endian ARM 32-bit (ARMv8 A-profile, non-Thumb)
    /// architecture descriptor.
    ///
    /// Uses the `ARM8_le` Sleigh spec with the `ARM_v45` processor spec, which
    /// matches the `-marm` compilation target in `fixtures/arch/arm.mk`.
    #[must_use]
    pub fn arm() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
            pspec: rsleigh::pspec::PSPEC_ARM_V45,
            endianness: Endianness::Little,
            preset: ArchPreset::Arm,
        }
    }

    /// Returns the little-endian AArch64 (ARM 64-bit) architecture descriptor.
    #[must_use]
    pub fn aarch64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64,
            pspec: rsleigh::pspec::PSPEC_AARCH64,
            endianness: Endianness::Little,
            preset: ArchPreset::Aarch64,
        }
    }

    /// Returns the big-endian AArch64 architecture descriptor.
    #[must_use]
    pub fn aarch64be() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64BE,
            pspec: rsleigh::pspec::PSPEC_AARCH64,
            endianness: Endianness::Big,
            preset: ArchPreset::Aarch64Be,
        }
    }

    /// Returns the big-endian MIPS-64 architecture descriptor.
    /// Used by Linux's N64 ABI (`mips64-linux-gnuabi64-gcc`).
    #[must_use]
    pub fn mipsbe64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS64BE,
            pspec: rsleigh::pspec::PSPEC_MIPS64,
            endianness: Endianness::Big,
            preset: ArchPreset::MipsBe64,
        }
    }

    /// Returns the little-endian MIPS-64 architecture descriptor.
    /// Used by Linux's N64 ABI (`mips64el-linux-gnuabi64-gcc`).
    #[must_use]
    pub fn mipsle64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS64LE,
            pspec: rsleigh::pspec::PSPEC_MIPS64,
            endianness: Endianness::Little,
            preset: ArchPreset::MipsLe64,
        }
    }

    /// Returns the big-endian PowerPC 32-bit architecture descriptor.
    /// Used by `powerpc-linux-gnu-gcc` (System V 32-bit ABI).
    #[must_use]
    pub fn ppc32be() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_PPC_32_BE,
            pspec: rsleigh::pspec::PSPEC_PPC_32,
            endianness: Endianness::Big,
            preset: ArchPreset::Ppc32Be,
        }
    }

    /// Returns the little-endian PowerPC 32-bit architecture descriptor.
    /// Used via `powerpc-linux-gnu-gcc -mlittle-endian` (uncommon Linux
    /// target, but the Sleigh spec exists and is symmetric with `ppc32be`).
    #[must_use]
    pub fn ppc32le() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_PPC_32_LE,
            pspec: rsleigh::pspec::PSPEC_PPC_32,
            endianness: Endianness::Little,
            preset: ArchPreset::Ppc32Le,
        }
    }

    /// Returns the big-endian PowerPC 64-bit architecture descriptor.
    /// Used by `powerpc64-linux-gnu-gcc` (ELFv1 ABI with function
    /// descriptors).  Uses the Power ISA + Altivec sla spec so Power7+
    /// scalar ops (`popcntw`, `popcntd`, `cntlzd`, `cnttzd`, …) and
    /// Altivec vector ops decode — the stripped `PPC_64_BE` spec
    /// rejects them with `Unable to resolve constructor`.
    #[must_use]
    pub fn ppc64be() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_PPC_64_ISA_ALTIVEC_BE,
            pspec: rsleigh::pspec::PSPEC_PPC_64,
            endianness: Endianness::Big,
            preset: ArchPreset::Ppc64Be,
        }
    }

    /// Returns the little-endian PowerPC 64-bit architecture descriptor.
    /// Used by `powerpc64le-linux-gnu-gcc` (ELFv2 ABI — no function
    /// descriptors, dot-prefixed symbols).  Uses the Power ISA + Altivec
    /// sla spec — see `ppc64be` for the rationale.
    #[must_use]
    pub fn ppc64le() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_PPC_64_ISA_ALTIVEC_LE,
            pspec: rsleigh::pspec::PSPEC_PPC_64,
            endianness: Endianness::Little,
            preset: ArchPreset::Ppc64Le,
        }
    }

    /// Returns the ARM Thumb-mode descriptor (32-bit ARM Cortex-M
    /// processors — Thumb-2 only).  Sleigh's `ARM8_le` spec decodes
    /// both ARM and Thumb instructions; the `ARMCORTEX` pspec selects
    /// Thumb-only Cortex-M decoding.
    ///
    /// Used with `arm-linux-gnueabihf-gcc -mthumb`.
    #[must_use]
    pub fn arm_thumb() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
            pspec: rsleigh::pspec::PSPEC_ARMCORTEX,
            endianness: Endianness::Little,
            preset: ArchPreset::ArmThumb,
        }
    }

    /// Returns the big-endian ARM 32-bit (ARMv8 A-profile, non-Thumb)
    /// architecture descriptor.
    ///
    /// Uses the `ARM8_be` Sleigh spec with the `ARM_v45` processor spec —
    /// the BE-instruction-encoding mirror of [`SleighArch::arm`].  ARM
    /// AAPCS is byte-order independent, so the same `arm_aapcs` calling
    /// convention pairs with both LE and BE.
    ///
    /// Used with `clang --target=armeb-linux-gnueabi` linking via the
    /// `arm-linux-gnueabihf` GNU `ld` (lld 14 has no `armelfb_linux_eabi`
    /// emulation; the GNU linker handles `-EB` via the BE BFD target).
    #[must_use]
    pub fn arm_be() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_ARM8_BE,
            pspec: rsleigh::pspec::PSPEC_ARM_V45,
            endianness: Endianness::Big,
            preset: ArchPreset::ArmBe,
        }
    }

    /// Probes Sleigh against an empty memory reader to extract this arch's
    /// register table.  Convenience for tests and other callers that need a
    /// `SleighRegs` without decoding any code.
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
