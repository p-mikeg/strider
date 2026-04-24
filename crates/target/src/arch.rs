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
/// convention to build an analyser for that target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    /// The `.sla` specification for the architecture's instruction set.
    pub sla_spec: rsleigh::sla_spec::SlaSpec,
    /// The `.pspec` processor specification (register and space definitions).
    pub pspec: rsleigh::pspec::PSpec,
    /// The byte order of this architecture.
    pub endianness: Endianness,
    /// The Sleigh register name of the hardware stack pointer.
    pub stack_ptr_reg_name: &'static str,
}

impl SleighArch {
    /// Returns the x86-64 (64-bit Intel/AMD) architecture descriptor.
    pub fn x86_64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86_64,
            pspec: rsleigh::pspec::PSPEC_X86_64,
            endianness: Endianness::Little,
            stack_ptr_reg_name: "RSP",
        }
    }

    /// Returns the x86 (32-bit Intel/AMD) architecture descriptor.
    pub fn x86() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86,
            pspec: rsleigh::pspec::PSPEC_X86,
            endianness: Endianness::Little,
            stack_ptr_reg_name: "ESP",
        }
    }

    /// Returns the big-endian MIPS-32 architecture descriptor.
    pub fn mipsbe32() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32BE,
            pspec: rsleigh::pspec::PSPEC_MIPS32,
            endianness: Endianness::Big,
            stack_ptr_reg_name: "sp",
        }
    }

    /// Returns the little-endian MIPS-32 architecture descriptor.
    pub fn mipsle32() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32LE,
            pspec: rsleigh::pspec::PSPEC_MIPS32,
            endianness: Endianness::Little,
            stack_ptr_reg_name: "sp",
        }
    }

    /// Returns the little-endian ARM 32-bit (ARMv8 A-profile, non-Thumb)
    /// architecture descriptor.
    ///
    /// Uses the `ARM8_le` Sleigh spec with the `ARM_v45` processor spec, which
    /// matches the `-marm` compilation target in `binary_tests/arch/arm.mk`.
    pub fn arm() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_ARM8_LE,
            pspec: rsleigh::pspec::PSPEC_ARM_V45,
            endianness: Endianness::Little,
            stack_ptr_reg_name: "sp",
        }
    }

    /// Returns the little-endian AArch64 (ARM 64-bit) architecture descriptor.
    pub fn aarch64() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64,
            pspec: rsleigh::pspec::PSPEC_AARCH64,
            endianness: Endianness::Little,
            stack_ptr_reg_name: "sp",
        }
    }

    /// Returns the big-endian AArch64 architecture descriptor.
    pub fn aarch64be() -> SleighArch {
        SleighArch {
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64BE,
            pspec: rsleigh::pspec::PSPEC_AARCH64,
            endianness: Endianness::Big,
            stack_ptr_reg_name: "sp",
        }
    }
}
