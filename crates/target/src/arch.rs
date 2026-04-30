/// The byte order used by an architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endianness {
    /// Least-significant byte at the lowest address (x86, AArch64 LE, …).
    Little,
    /// Most-significant byte at the lowest address (MIPS BE, AArch64 BE, …).
    Big,
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
    /// The `.sla` specification for the architecture's instruction set.
    pub sla_spec: rsleigh::sla_spec::SlaSpec,
    /// The `.pspec` processor specification (register and space definitions).
    pub pspec: rsleigh::pspec::PSpec,
    /// The byte order of this architecture.
    pub endianness: Endianness,
}

impl SleighArch {
    /// Returns the x86-64 (64-bit Intel/AMD) architecture descriptor.
    #[must_use]
    pub fn x86_64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86_64,
            pspec: rsleigh::pspec::PSPEC_X86_64,
            endianness: Endianness::Little,
        }
    }

    /// Returns the x86 (32-bit Intel/AMD) architecture descriptor.
    #[must_use]
    pub fn x86() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86,
            pspec: rsleigh::pspec::PSPEC_X86,
            endianness: Endianness::Little,
        }
    }

    /// Returns the big-endian MIPS-32 architecture descriptor.
    #[must_use]
    pub fn mipsbe32() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32BE,
            pspec: rsleigh::pspec::PSPEC_MIPS32,
            endianness: Endianness::Big,
        }
    }

    /// Returns the little-endian MIPS-32 architecture descriptor.
    #[must_use]
    pub fn mipsle32() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32LE,
            pspec: rsleigh::pspec::PSPEC_MIPS32,
            endianness: Endianness::Little,
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
        }
    }

    /// Returns the little-endian AArch64 (ARM 64-bit) architecture descriptor.
    #[must_use]
    pub fn aarch64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64,
            pspec: rsleigh::pspec::PSPEC_AARCH64,
            endianness: Endianness::Little,
        }
    }

    /// Returns the big-endian AArch64 architecture descriptor.
    #[must_use]
    pub fn aarch64be() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64BE,
            pspec: rsleigh::pspec::PSPEC_AARCH64,
            endianness: Endianness::Big,
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
