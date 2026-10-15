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
            /// Every variant.
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
    /// constructor.  Only the roster-sweeping tests need the inverse; a
    /// caller holding a preset names the constructor directly.
    #[cfg(any(test, feature = "test-util"))]
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

/// Whether the sla declares a committed context var as flowing, which decides
/// what [`SleighArch::flow_commit_vars`] re-derives its value from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlowCommit {
    /// Confined to one address, so the pspec default is the erase target.
    NoFlow,
    /// Painted forward to persist, so the predecessor's value is.
    Flowing,
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

    /// The context vars whose value AT AN ADDRESS a constructor commits, and
    /// which therefore have to be re-derived from this build's own decode
    /// rather than read out of the engine.
    ///
    /// Each var paired with whether the sla declares it FLOWING, which decides
    /// what the erase re-derives FROM.
    ///
    /// A `noflow` commit covers one address
    /// (`ParserContext::applyCommits` gives it `setContextRegion(a, a+1)`), so
    /// its value at `inst_next` is a fact of one instruction's decode and the
    /// pspec default is the right erase target. A FLOWING commit is
    /// `setContextChangePoint`: it paints forward because it is MEANT to
    /// persist, so erasing it to the default would destroy it at every
    /// `inst_next` and re-establish it only where the committing instruction
    /// happens to be the predecessor -- which decoded the second ThumbEE
    /// instruction after an ARM `enterx` as plain Thumb. A flowing var is
    /// therefore erased to the value in force at the PREDECESSOR, reproducing
    /// the flow, and the re-lift then re-applies any commit of its own.
    ///
    /// Both classes must be listed. `Builder::committed_next` licenses
    /// `Builder::enqueue` to trust a snapshot of every flowing var at a
    /// recorded `inst_next`, and `FlowVars::pin_at` cannot clean one up: a
    /// forward paint stops at the next CHANGE POINT of that var, which is
    /// exactly where an earlier build's commit sits. Leaving ARM `TEEMode`
    /// out let a stale ThumbEE mode survive into a later `build_cfg` and drop
    /// a `Store`.
    ///
    /// Named directly, not via the flow-var views: ARM's `condit` is a
    /// `noflow` field spanning bits 5..13, so one name clears the whole
    /// IT-block state, where the six overlapping flowing views that tile those
    /// bits would each need separate care.
    ///
    /// ARM `LRset` is `noflow` and committed at `inst_next` too, but belongs to
    /// [`Self::transient_decode_vars`], which erases it to the default WITHOUT
    /// a re-lift because a `mov lr, pc` state must not survive a region
    /// boundary at all.
    ///
    /// Only ARM among the presets here. x86 and AArch64 contain no `globalset`
    /// at all; MIPS commits `ISA_MODE` only at a `jalx` call target, which
    /// analysis never decodes, and its `PAIR_INSTRUCTION_FLAG` / `ext_delay`
    /// are `noflow` and go to [`Self::transient_decode_vars`]; PowerPC's
    /// `linkreg` IS committed to a differing value by `mtlr`
    /// (`ppc_embedded.sinc:155`) and is flowing, but every constructor it
    /// selects emits p-code identical to its `linkreg=0` twin, so a stale
    /// value cannot change the lift.
    #[must_use]
    pub fn flow_commit_vars(&self) -> &'static [(&'static str, FlowCommit)] {
        match self.preset {
            ArchPreset::Arm
            | ArchPreset::ArmBe
            | ArchPreset::ArmBeKernel
            | ArchPreset::ArmThumb => &[
                ("condit", FlowCommit::NoFlow),
                ("TEEMode", FlowCommit::Flowing),
            ],
            _ => &[],
        }
    }

    /// Whether any constructor in this sla `globalset`s a FLOWING context
    /// variable at an address another instruction then decodes in.
    ///
    /// Only the ARM family. Verified against every language dir rsleigh
    /// ships: x86 and AArch64 contain no `globalset` at all; MIPS commits
    /// `ISA_MODE` only at a `jalx` CALL target, which analysis never decodes;
    /// and PowerPC's `linkreg` IS committed to a differing value by `mtlr`
    /// (`ppc_embedded.sinc:155`) but is itself flowing, so a region start's
    /// pin covers it, and every constructor it selects emits p-code identical
    /// to its `linkreg=0` twin. ARM commits `TEEMode` and the IT-block
    /// `condit` group at `inst_next`, and `TMode` both at `inst_next` and at a
    /// branch TARGET (`ARMinstructions.sinc:677`,
    /// `ARMTHUMBinstructions.sinc:1544`).
    ///
    /// Reported, not used: the per-instruction re-derivation is gated on
    /// [`Self::flow_commit_vars`] being non-empty, which is the narrower
    /// `noflow`-only set.
    #[must_use]
    pub fn commits_flow_context(&self) -> bool {
        matches!(
            self.preset,
            ArchPreset::Arm | ArchPreset::ArmBe | ArchPreset::ArmBeKernel | ArchPreset::ArmThumb
        )
    }

    /// Whether the sla decodes a branch together with its delay slot, one
    /// `lift_one` spanning both.
    #[must_use]
    pub fn has_delay_slots(&self) -> bool {
        matches!(
            self.preset,
            ArchPreset::MipsBe32
                | ArchPreset::MipsLe32
                | ArchPreset::MipsBe64
                | ArchPreset::MipsLe64
        )
    }

    /// Context vars the sla declares `noflow` that still change which
    /// constructor matches, so a value committed by one function is a decode
    /// change for the next on a reused engine and a cold entry must clear them.
    ///
    /// Erased to the pspec default with NO re-lift, which is what separates
    /// these from [`Self::flow_commit_vars`]: a `mov lr, pc` state must not
    /// survive a region boundary at all, where a committed IT state has to be
    /// re-derived rather than cleared.
    ///
    /// `FlowVars` cannot see these: it takes the sla's FLOWING set, and these
    /// are excluded from it by definition. ARM's `LRset` picks `call [pc]` over
    /// `goto [pc]` for `bx` (`ARMinstructions.sinc`), and `REToverride` /
    /// `CALLoverride` reclassify a return and a call the same way. MIPS's
    /// `PAIR_INSTRUCTION_FLAG` (`mips.sinc:412`) selects the
    /// `lwl`/`swl`/`ldl`/`sdl` constructor that does the whole unaligned access
    /// against the one that does half, and `ext_delay` (`mips.sinc:450`) is
    /// subtracted from the PC-relative address MIPS16e `OFF_M16PC` computes
    /// (`mips16.sinc:164`). `LRset`, `PAIR_INSTRUCTION_FLAG` and `ext_delay`
    /// are committed by `globalset`, so a commit outlives the function that
    /// made it. `REToverride` / `CALLoverride` are only ever read; clearing
    /// them costs nothing and keeps the list one rule.
    #[must_use]
    pub fn transient_decode_vars(&self) -> &'static [&'static str] {
        match self.preset {
            ArchPreset::MipsBe32
            | ArchPreset::MipsLe32
            | ArchPreset::MipsBe64
            // `ext_delay` is written by the five MIPS16e branch/call
            // constructors (`mips16.sinc:520,528,542,550,557`), reachable only
            // at `ISA_MODE=1`.
            | ArchPreset::MipsLe64 => &["PAIR_INSTRUCTION_FLAG", "ext_delay"],
            ArchPreset::Arm
            | ArchPreset::ArmBe
            | ArchPreset::ArmBeKernel
            // `condit` is deliberately absent: it belongs to
            // `flow_commit_vars`, which RE-DERIVES it by re-lifting the owning
            // instruction. These are cleared with no re-lift, including at a
            // region start, which destroys an IT state the predecessor
            // legitimately committed.
            | ArchPreset::ArmThumb => &["LRset", "REToverride", "CALLoverride"],
            _ => &[],
        }
    }

    /// Whether a `CallOther` whose pcode output is a sub-register of
    /// `container` may be lifted as a write of that slice alone, leaving the
    /// enclosing register's other bytes as they were.
    ///
    /// It may only where the machine leaves them alone and the sla says so by
    /// writing the slice alone: x86 8- and 16-bit GPR destinations and legacy
    /// SSE into a VEX-widened register (a 32-bit destination zero-extends, and
    /// the sla emits that zext itself).
    ///
    /// The x87 `ST` registers, the only 10-byte register on either x86 preset,
    /// are not among them: their `MM` slices are a differently-typed view, and
    /// an MMX write redefines the sign and exponent a slice write would
    /// preserve. `ST0`/`ST1` are float return registers, so those bits would be
    /// returned.
    ///
    /// Elsewhere the slice write would read bytes the machine redefines.
    /// AArch64 zeroes them and the fork's sla emits the zext, so a slice write
    /// is sound there, but it reads the incoming register first and that read
    /// becomes a phantom argument carrier. ARM32, MIPS and PowerPC keep the
    /// whole-container clobber too, which is opaque rather than wrong.
    #[must_use]
    pub fn callother_output_is_exact_in(&self, container: &rsleigh::Vn) -> bool {
        matches!(self.preset, ArchPreset::X86 | ArchPreset::X86_64) && container.size != 10
    }

    /// Registers the sla declares for its own semantics rather than as
    /// processor state: scratch a constructor writes and reads within one
    /// instruction, or storage the decompiler fakes.  No callee can read them.
    ///
    /// ARM `mult_addr` / `mult_dat8` / `mult_dat16` walk a `push` / `pop` /
    /// `ldm` / `stm` over its list (`ARM.sinc:13-16`), so `mult_addr` holds a
    /// stack address after every `push`, and `tmpNG`.. hold flags mid-compute
    /// (`:11`).  AArch64 `tmp_ldWn`.. carry a loaded or stored value
    /// (`AARCH64instructions.sinc:419-422`), `VecMemAddr` / `VectorSelem` /
    /// `VecRegAddr` an address a vector load or store steps (`:68-71`),
    /// `glob_mask32` / `glob_mask64` a computed mask (`:64-65`), `TMPZ1`.. SIMD
    /// intermediates (`:543`), `tmpCY`.. flags (`:62`).  x86 `xmmTmp1` /
    /// `xmmTmp2` (`ia.sinc:140`).  MIPS `tsp` steps the stack over a microMIPS
    /// `lwm` / `swm` list (`mips.sinc:368`, `mipsmicro.sinc:430`).  PowerPC
    /// `r2Save` is fake storage for the TOC (`ppc_common.sinc:72`), and
    /// `RESERVE_ADDRESS` the load-reservation address `lwarn` records
    /// (`:53`), which only a later `stwcx` on the same thread consults.
    #[must_use]
    pub fn internal_registers(&self) -> &'static [&'static str] {
        match self.preset {
            ArchPreset::Arm
            | ArchPreset::ArmBe
            | ArchPreset::ArmBeKernel
            | ArchPreset::ArmThumb => &[
                "mult_addr",
                "mult_dat8",
                "mult_dat16",
                "tmpNG",
                "tmpZR",
                "tmpCY",
                "tmpOV",
            ],
            ArchPreset::Aarch64 | ArchPreset::Aarch64Be => &[
                "tmp_ldWn",
                "tmp_ldXn",
                "tmp_stWn",
                "tmp_stXn",
                "VecMemAddr",
                "VectorSelem",
                "VecRegAddr",
                "glob_mask32",
                "glob_mask64",
                "TMPZ1",
                "TMPZ2",
                "TMPZ3",
                "TMPZ4",
                "TMPZ5",
                "TMPZ6",
                "tmpCY",
                "tmpOV",
                "tmpNG",
                "tmpZR",
            ],
            ArchPreset::X86 | ArchPreset::X86_64 => &["xmmTmp1", "xmmTmp2"],
            ArchPreset::MipsBe32
            | ArchPreset::MipsLe32
            | ArchPreset::MipsBe64
            | ArchPreset::MipsLe64 => &["tsp"],
            ArchPreset::Ppc32Be
            | ArchPreset::Ppc32Le
            | ArchPreset::Ppc64Be
            | ArchPreset::Ppc64Le => &["r2Save", "RESERVE_ADDRESS"],
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
        /// `powerpc64le-linux-gnu-gcc`, ELFv2 ABI: no function descriptors and
        /// no dot-prefixed symbols.  Power ISA + Altivec sla spec, see
        /// `ppc64be`.
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

    /// This arch's register table, read off a Sleigh over an empty image.
    #[cfg(any(test, feature = "test-util"))]
    pub fn probe_regs(self) -> anyhow::Result<rsleigh::SleighRegs> {
        let probe = rsleigh::mem_readers::BufMemReader::new(Vec::<u8>::new(), 0);
        let sleigh = rsleigh::Sleigh::new(self.sla_spec, self.pspec, probe)?;
        Ok(sleigh.regs()?)
    }
}

#[cfg(test)]
mod tests {
    use super::{ArchPreset, SleighArch};

    /// The two lists are one fact. `commits_flow_context` reports it,
    /// `flow_commit_vars` acts on it, and nothing else ties them together, so
    /// a preset added to one and not the other would decode against whatever
    /// an earlier build left.
    #[test]
    fn a_preset_that_commits_flow_context_names_the_vars_to_re_derive() {
        for preset in ArchPreset::ALL {
            let arch = preset.arch();
            assert_eq!(
                arch.commits_flow_context(),
                !arch.flow_commit_vars().is_empty(),
                "{preset:?} reports one answer and lists the other"
            );
        }
    }

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

    /// The rule on [`super::SleighArch::callother_output_is_exact_in`], pinned
    /// per preset and over the x87 exclusion within x86.
    #[test]
    fn callother_output_is_exact_only_where_the_machine_preserves_the_rest() {
        use super::ArchPreset;
        // RAX and ST0.
        let gpr = rsleigh::Vn {
            addr_space: rsleigh::VnSpace::REGISTER,
            addr_off: 0,
            size: 8,
        };
        let st0 = rsleigh::Vn { size: 10, ..gpr };
        for preset in ArchPreset::ALL {
            let expected = matches!(preset, ArchPreset::X86 | ArchPreset::X86_64);
            assert_eq!(
                preset.arch().callother_output_is_exact_in(&gpr),
                expected,
                "{preset:?}",
            );
            assert!(
                !preset.arch().callother_output_is_exact_in(&st0),
                "{preset:?} x87 container",
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
