mod decode_tables;
mod endian;
mod names;
#[cfg(test)]
mod tests;

pub use decode_tables::{DecodeTables, FlowCommit};
pub use endian::{ByteOrder, Endianness};
pub use names::ArchNames;

use crate::family::{self, DecodeFacts, Family};

/// Emits `ArchPreset`, its [`ArchPreset::ALL`] roster and every per-preset
/// fact from one row list, so no roster can omit a preset and no per-preset
/// answer can drift from the variant it belongs to. Each row's doc lands on
/// the variant and on its constructor alike.
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

            /// The family this preset belongs to.
            #[must_use]
            pub(crate) const fn family(self) -> &'static Family {
                match self { $(Self::$variant => &family::$family,)+ }
            }

            /// The name of the family this preset belongs to: `"x86"`,
            /// `"arm32"`, `"aarch64"`, `"mips"` or `"ppc"`.
            #[must_use]
            pub const fn family_name(self) -> &'static str {
                self.family().name
            }

            /// The Ghidra language definitions file this preset is a row of,
            /// under the vendored processors tree, e.g.
            /// `"x86/data/languages/x86.ldefs"`. Its first component is the
            /// processor directory.
            #[must_use]
            pub const fn ghidra_ldefs(self) -> &'static str {
                self.family().ghidra_ldefs
            }

            /// The Ghidra language id this preset is.
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
            ///
            /// Only the roster-sweeping tests need the inverse; a caller
            /// holding a preset names the constructor directly.
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
                    // A `static`, not the `const` itself: a const's bytes are
                    // emitted again wherever this constructor is inlined.
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
    /// Legacy **BE32** ARM 32-bit: big-endian instructions AND data,
    /// pre-ARMv6.
    ///
    /// Modern ARMv6+ big-endian Linux is **BE8** (little-endian
    /// instructions, big-endian data, flagged `EF_ARM_BE8`), not BE32.
    /// This spec byte-reverses every instruction word and fails almost
    /// every decode on such a binary; use [`SleighArch::arm_be_kernel`].
    ///
    /// The sla still decodes NEON, which no BE32 core has (ARMv7 removed
    /// BE32), and disagrees with itself on lane order there: a lane load
    /// or store (`vld1.32 {d0[0]}`) addresses lane 0 at the D register's
    /// high word, a core-register `vmov` (`vmov.32 r0, d0[0]`) at its low
    /// word.  NEON lane values under this preset are not meaningful.
    ArmBe => "arm_be", arm_be, ARM32, SLA_SPEC_ARM8_BE, PSPEC_ARMT, Big, Big, "ARM:BE:32:v8";
    /// **BE8** ARM 32-bit (GHIDRA's `ARM:LEBE:32`): little-endian
    /// instructions, big-endian data, i.e. modern ARMv6+ big-endian.
    /// Every ARMv6+ big-endian Linux target, kernel and userland, is this.
    ///
    /// `SLA_SPEC_ARM8_BE8` is `SLA_SPEC_ARM8_LE` with the transfers
    /// whose bytes depend on the data order, the `vld1` / `vst1`
    /// multiple-element forms, made one per element.
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
    /// `powerpc64-linux-gnu-gcc`, ELFv1 ABI with function descriptors.
    /// Needs the Power ISA + Altivec sla spec: the stripped `PPC_64_BE`
    /// spec rejects Power7+ scalar ops (`popcntw`, `popcntd`, `cntlzd`,
    /// `cnttzd`) and Altivec vector ops with `Unable to resolve
    /// constructor`.
    Ppc64Be => "ppc64be", ppc64be, PPC, SLA_SPEC_PPC_64_ISA_ALTIVEC_BE, PSPEC_PPC_64, Big, Big, "PowerPC:BE:64:A2ALT";
    /// `powerpc64le-linux-gnu-gcc`, ELFv2 ABI: no function descriptors and
    /// no dot-prefixed symbols.  Power ISA + Altivec sla spec, see
    /// [`SleighArch::ppc64be`].
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
    /// A Sleigh over `reader` from this preset's sla and pspec: the one place
    /// a Sleigh is built for an arch.
    ///
    /// # Errors
    ///
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

    /// The byte order of the Sleigh REGISTER space, which the sla fixes at
    /// compile time via `ENDIAN` and which is NOT always the data order.
    ///
    /// `arm_be_kernel` is BE8: a little-endian sla (little-endian instruction
    /// encoding) with big-endian data, mirroring GHIDRA's `ARM:LEBE:32`. Its
    /// register block therefore took the `@if ENDIAN == "little"` branch of
    /// `ARM.sinc`, where `d0` is the LOW half of `q0`; the big-endian branch
    /// reverses the name lists instead. Sub-register slicing and REGISTER-space
    /// loads and stores must follow this, while every other load and store
    /// follows [`Self::endianness`].
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

    /// The context variable holding the address-low-bit ISA mode, or `None` on
    /// arches with no such mode (they decode from the pspec default).
    ///
    /// `arm_thumb` is included: it is `-mthumb` over the full-ARM-state
    /// `SLA_SPEC_ARM8_LE`, so ARM-state code (gcc's `call_weak_fn`, veneers)
    /// still appears at even addresses and a resolved `bx` still interworks.
    #[must_use]
    pub fn isa_mode_var(&self) -> Option<&'static str> {
        self.facts().isa_mode_var
    }

    /// The context vars whose value AT AN ADDRESS a constructor commits, and
    /// which therefore have to be re-derived from this build's own decode
    /// rather than read out of the engine.
    ///
    /// What the erase re-derives FROM depends on whether the sla declares the
    /// var FLOWING, which [`crate::DecodeTables::flow_commit`] records.
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
    /// Both classes must be listed. The CFG build trusts a snapshot of every
    /// flowing var at a recorded `inst_next`, and a pin cannot clean one up: a
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
    /// at all; MIPS commits `ISA_MODE` only at a `jalx` call target, an
    /// ISA-mode var the CFG build holds per region instead (a callee below the
    /// caller paints the caller's bytes), and its `PAIR_INSTRUCTION_FLAG` / `ext_delay`
    /// are `noflow` and go to [`Self::transient_decode_vars`]; PowerPC's
    /// `linkreg` IS committed to a differing value by `mtlr`
    /// (`ppc_embedded.sinc:155`) and is flowing, but every constructor it
    /// selects emits p-code identical to its `linkreg=0` twin, so a stale
    /// value cannot change the lift, which
    /// `a_stale_linkreg_cannot_change_a_powerpc_lift` pins.
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

    /// Context vars the sla declares `noflow` that still change which
    /// constructor matches, so a value committed by one function is a decode
    /// change for the next on a reused engine and a cold entry must clear them.
    ///
    /// Erased to the pspec default with NO re-lift, which is what separates
    /// these from [`Self::flow_commit_vars`]: a `mov lr, pc` state must not
    /// survive a region boundary at all, where a committed IT state has to be
    /// re-derived rather than cleared.
    ///
    /// The decoder's flowing-context snapshots and pins cannot see these: they
    /// cover the sla's FLOWING set, which excludes these by definition. ARM's
    /// `LRset` picks `call [pc]` over `goto [pc]` for `bx`
    /// (`ARMinstructions.sinc`), and `REToverride` / `CALLoverride` reclassify
    /// a return and a call the same way. MIPS's `PAIR_INSTRUCTION_FLAG`
    /// (`mips.sinc:412`) selects the `lwl`/`swl`/`ldl`/`sdl` constructor that
    /// does the whole unaligned access against the one that does half, and
    /// `ext_delay` (`mips.sinc:450`) is subtracted from the PC-relative address
    /// MIPS16e `OFF_M16PC` computes (`mips16.sinc:164`). `LRset`,
    /// `PAIR_INSTRUCTION_FLAG` and `ext_delay` are committed by `globalset`, so
    /// a commit outlives the function that made it. `REToverride` /
    /// `CALLoverride` are only ever read; clearing them costs nothing and keeps
    /// the list one rule.
    #[must_use]
    pub fn transient_decode_vars(&self) -> &'static [&'static str] {
        self.facts().transient_decode_vars
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
        self.facts().callother_output_exact && container.size != 10
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
    /// `RESERVE_ADDRESS` the load-reservation address `lwarx` records
    /// (`:53`), which only a later `stwcx` on the same thread consults.
    #[must_use]
    pub fn internal_registers(&self) -> &'static [&'static str] {
        self.facts().internal_registers
    }

    /// REGISTER-space byte ranges, `[start, end)`, whose value changes between
    /// two reads with nothing writing it: a free-running counter such as
    /// AArch64 `cntvct_el0`, the PowerPC timebase or MIPS CP0 `Count`.
    ///
    /// The pspec's `<volatile>` ranges and `volatile` registers, which make
    /// every AArch64 system register volatile, plus the family's own
    /// additions, less the family's exclusions (AArch64 `sp_el0`).
    ///
    /// # Errors
    ///
    /// A volatile range outside the REGISTER space, or a name this arch's
    /// register table does not declare.
    pub fn volatile_register_ranges(&self) -> crate::Result<Vec<core::ops::Range<u64>>> {
        let regs = self.names().regs();
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

    /// The ISA-mode context a cold entry at `entry_addr` decodes in, as
    /// `(context_var, value)`: the address low bit set means the alternate ISA
    /// (ARM Thumb, MIPS16e), clear means the base one. The instruction itself
    /// is at `entry_addr & !1`. Which alternate ISA a MIPS preset means is
    /// fixed by its pspec, see [`Self::isa_mode_var`].
    pub fn entry_mode_context(&self, entry_addr: u64) -> Option<(&'static str, u32)> {
        self.isa_mode_var()
            .map(|var| (var, u32::from(entry_addr & 1 == 1)))
    }

    /// This arch's register table, a copy of [`SleighArch::names`]'s.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn probe_regs(self) -> rsleigh::SleighRegs {
        self.names().regs().clone()
    }
}
