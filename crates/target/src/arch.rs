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
/// Pass a `SleighArch` to [`crate::Analyzer::new`] along with the calling
/// convention to build an analyser for that target.  The calling convention
/// owns the stack-pointer register name (see
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
    /// matches the `-marm` compilation target in `binary_tests/arch/arm.mk`.
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
}
