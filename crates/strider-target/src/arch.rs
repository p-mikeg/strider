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
    /// `0x0403_0201` little-endian, `0x0102_0304` big-endian.
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

/// Emits `ArchPreset` and the [`ArchPreset::ALL`] roster from one variant
/// list, so a roster cannot omit a variant.
macro_rules! arch_presets {
    ($($(#[$doc:meta])* $variant:ident),+ $(,)?) => {
        /// One variant per [`SleighArch`] preset.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ArchPreset {
            $($(#[$doc])* $variant,)+
        }

        impl ArchPreset {
            /// Every variant. Rosters iterate this instead of re-enumerating.
            pub const ALL: &'static [ArchPreset] = &[$(ArchPreset::$variant,)+];
        }
    };
}

arch_presets! {
    X86,
    X86_64,
    Arm,
    ArmBe,
    /// BE8 ARM (little-endian instructions, big-endian data), i.e. modern
    /// ARMv6+ big-endian.
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

impl ArchPreset {
    /// The arch this preset names, inverse of [`SleighArch::preset`].
    ///
    /// Exhaustive, so a new variant fails to compile until it has a
    /// constructor.
    #[must_use]
    pub fn arch(self) -> SleighArch {
        match self {
            Self::X86 => SleighArch::x86(),
            Self::X86_64 => SleighArch::x86_64(),
            Self::Arm => SleighArch::arm(),
            Self::ArmBe => SleighArch::arm_be(),
            Self::ArmBeKernel => SleighArch::arm_be_kernel(),
            Self::ArmThumb => SleighArch::arm_thumb(),
            Self::Aarch64 => SleighArch::aarch64(),
            Self::Aarch64Be => SleighArch::aarch64be(),
            Self::MipsBe32 => SleighArch::mipsbe32(),
            Self::MipsLe32 => SleighArch::mipsle32(),
            Self::MipsBe64 => SleighArch::mipsbe64(),
            Self::MipsLe64 => SleighArch::mipsle64(),
            Self::Ppc32Be => SleighArch::ppc32be(),
            Self::Ppc32Le => SleighArch::ppc32le(),
            Self::Ppc64Be => SleighArch::ppc64be(),
            Self::Ppc64Le => SleighArch::ppc64le(),
        }
    }
}

/// The Sleigh configuration describing one target architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    sla_spec: rsleigh::sla_spec::SlaSpec,
    pspec: rsleigh::pspec::PSpec,
    endianness: Endianness,
    pub(crate) preset: ArchPreset,
}

/// Emits a `pub fn $name() -> SleighArch` preset constructor.
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

    /// The byte order of the Sleigh REGISTER space, which the sla fixes at
    /// compile time via `ENDIAN` and which is NOT always the data order.
    ///
    /// `arm_be_kernel` is BE8: a little-endian sla (little-endian instruction
    /// encoding) with big-endian data, mirroring GHIDRA's `ARM:LEBE:32`. Its
    /// register block therefore took the `@if ENDIAN == "little"` branch of
    /// `ARM.sinc`, where `d0` is the LOW half of `q0`; the big-endian branch
    /// reverses the name lists instead. Sub-register slicing must follow this,
    /// while loads and stores follow [`Self::endianness`].
    pub fn register_endianness(&self) -> Endianness {
        match self.preset {
            ArchPreset::ArmBeKernel => Endianness::Little,
            _ => self.endianness,
        }
    }

    pub fn preset(&self) -> ArchPreset {
        self.preset
    }

    /// The context variable holding the address-low-bit ISA mode, or `None` on
    /// arches with no such mode (they decode from the pspec default).
    ///
    /// `arm_thumb` is included: it is `-mthumb` over the full-ARM-state
    /// `SLA_SPEC_ARM8_LE`, so ARM-state code (gcc's `call_weak_fn`, veneers)
    /// still appears at even addresses and a resolved `bx` still interworks.
    #[must_use]
    pub fn isa_mode_var(&self) -> Option<&'static str> {
        match self.preset {
            ArchPreset::Arm
            | ArchPreset::ArmBe
            | ArchPreset::ArmBeKernel
            | ArchPreset::ArmThumb => Some("TMode"),
            // The low bit selects the alternate ISA, which `RELP` fixes: every
            // MIPS pspec here pins it to 1, so that ISA is MIPS16e. rsleigh
            // ships `PSPEC_MIPS32MICRO` / `PSPEC_MIPS64MICRO` for `RELP=0`
            // (microMIPS) and no preset uses them, so a microMIPS image decodes
            // its odd-addressed functions against MIPS16e tables.
            ArchPreset::MipsBe32
            | ArchPreset::MipsLe32
            | ArchPreset::MipsBe64
            | ArchPreset::MipsLe64 => Some("ISA_MODE"),
            _ => None,
        }
    }

    /// Context vars the sla declares `noflow` that still change which
    /// constructor matches, so a value committed by one function is a decode
    /// change for the next on a reused engine and a cold entry must clear them.
    ///
    /// `FlowVars` cannot see these: it takes the sla's FLOWING set, and these
    /// are excluded from it by definition. ARM's `LRset` picks `call [pc]` over
    /// `goto [pc]` for `bx` (`ARMinstructions.sinc`), and `REToverride` /
    /// `CALLoverride` reclassify a return and a call the same way.
    #[must_use]
    pub fn transient_decode_vars(&self) -> &'static [&'static str] {
        match self.preset {
            ArchPreset::Arm
            | ArchPreset::ArmBe
            | ArchPreset::ArmBeKernel
            // `condit` is deliberately absent: its bits 5..13 are tiled by the
            // FLOWING `itmode` / `cond_base` / `cond_full` / `cond_true` /
            // `cond_mask` / `cond_shft`, which `FlowVars` already resets, so
            // listing it would write those same bits a second time. Being
            // `noflow` its real lifetime is one instruction, which a
            // forward-painting context write cannot express.
            | ArchPreset::ArmThumb => &["LRset", "REToverride", "CALLoverride"],
            _ => &[],
        }
    }

    /// The ISA-mode context a cold entry at `entry_addr` decodes in, as
    /// `(context_var, value)`: the address low bit set means the alternate ISA
    /// (ARM Thumb, MIPS16e), clear means the base one. The instruction itself
    /// is at `entry_addr & !1`. Which alternate ISA a MIPS preset means is
    /// fixed by its pspec, see [`Self::isa_mode_var`].
    pub fn entry_mode_context(&self, entry_addr: u64) -> Option<(&'static str, u32)> {
        self.isa_mode_var()
            .map(|var| (var, u32::from(entry_addr & 1 == 1)))
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
        /// ARM 32-bit little-endian, non-Thumb.
        arm => SLA_SPEC_ARM8_LE, PSPEC_ARMT, Little, Arm
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
        /// `powerpc-linux-gnu-gcc -mlittle-endian`.
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
        /// ARM 32-bit `-mthumb`: Cortex-M pspec defaults over the full-ARM-state
        /// sla, so ARM state stays reachable (`arm-linux-gnueabihf-gcc -mthumb`).
        arm_thumb => SLA_SPEC_ARM8_LE, PSPEC_ARMCORTEX, Little, ArmThumb
    }

    arch_ctor! {
        /// Legacy **BE32** ARM 32-bit: big-endian instructions AND data,
        /// pre-ARMv6.
        ///
        /// Modern ARMv6+ big-endian Linux is **BE8** (little-endian
        /// instructions, big-endian data, flagged `EF_ARM_BE8`), not BE32.
        /// This spec byte-reverses every instruction word and fails almost
        /// every decode on such a binary; use [`SleighArch::arm_be_kernel`].
        arm_be => SLA_SPEC_ARM8_BE, PSPEC_ARMT, Big, ArmBe
    }

    arch_ctor! {
        /// **BE8** ARM 32-bit (GHIDRA's `ARM:LEBE:32`): little-endian
        /// instructions, big-endian data.  Every ARMv6+ big-endian Linux
        /// target, kernel and userland, is this.
        arm_be_kernel => SLA_SPEC_ARM8_LE, PSPEC_ARMT, Big, ArmBeKernel
    }

    /// Extracts this arch's register table by probing Sleigh against an
    /// empty memory reader.
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

#[cfg(test)]
mod tests {
    use super::SleighArch;

    /// `arm_thumb` is `arm-linux-gnueabihf-gcc -mthumb` over the full-ARM-state
    /// `SLA_SPEC_ARM8_LE`, not a Cortex-M: its own fixture
    /// (`fixtures/out/arm_thumb/arithmetic.elf`) carries `$a` ARM-state mapping
    /// symbols and an even-addressed `FUNC call_weak_fn` at 0x530. Pinning
    /// `TMode = 1` decodes those as garbage Thumb with no error, and withholding
    /// the carry var leaves a resolved interworking branch unable to recover ARM.
    #[test]
    fn arm_thumb_honours_the_address_low_bit_and_carries_its_mode() {
        assert_eq!(
            SleighArch::arm_thumb().entry_mode_context(0x0530),
            Some(("TMode", 0)),
            "an even entry is ARM state, which this sla and preset both have"
        );
        assert_eq!(
            SleighArch::arm_thumb().entry_mode_context(0x05f9),
            Some(("TMode", 1)),
            "a Thumb function symbol carries the Thumb bit"
        );
        assert_eq!(SleighArch::arm_thumb().isa_mode_var(), Some("TMode"));
    }

    /// Every preset carries the mode it decodes with, and only the ARM and
    /// MIPS ones have a mode at all.
    #[test]
    fn isa_mode_var_matches_entry_mode_context_on_every_preset() {
        use super::ArchPreset;
        for preset in ArchPreset::ALL {
            let arch = preset.arch();
            assert_eq!(
                arch.isa_mode_var(),
                arch.entry_mode_context(0).map(|(v, _)| v),
                "{preset:?} must carry the mode it decodes with",
            );
            let expected = matches!(
                preset,
                ArchPreset::Arm
                    | ArchPreset::ArmBe
                    | ArchPreset::ArmBeKernel
                    | ArchPreset::ArmThumb
                    | ArchPreset::MipsBe32
                    | ArchPreset::MipsLe32
                    | ArchPreset::MipsBe64
                    | ArchPreset::MipsLe64
            );
            assert_eq!(
                arch.isa_mode_var().is_some(),
                expected,
                "{preset:?} ISA-mode presence",
            );
        }
    }

    /// Every constructor stamps its own discriminator, so a copy-paste in
    /// `arch_ctor!` cannot make two presets indistinguishable.
    #[test]
    fn preset_round_trips_through_its_constructor() {
        for preset in super::ArchPreset::ALL {
            assert_eq!(preset.arch().preset(), *preset);
        }
    }

    #[test]
    fn entry_mode_context_maps_low_bit_per_arch() {
        // MIPS: the low bit selects the alternate ISA via ISA_MODE.
        assert_eq!(
            SleighArch::mipsbe32().entry_mode_context(0x0040_0a90),
            Some(("ISA_MODE", 0))
        );
        assert_eq!(
            SleighArch::mipsle64().entry_mode_context(0x0040_0a91),
            Some(("ISA_MODE", 1))
        );
        // ARM: the low bit is the Thumb bit via TMode.
        assert_eq!(
            SleighArch::arm().entry_mode_context(0x1001),
            Some(("TMode", 1))
        );
        assert_eq!(
            SleighArch::arm_thumb().entry_mode_context(0x1001),
            Some(("TMode", 1))
        );
        assert_eq!(
            SleighArch::arm_thumb().entry_mode_context(0x1000),
            Some(("TMode", 0))
        );
        // Arches with no address-encoded entry mode.
        assert_eq!(SleighArch::x86_64().entry_mode_context(0x1000), None);
        assert_eq!(SleighArch::ppc32be().entry_mode_context(0x1001), None);
    }
}
