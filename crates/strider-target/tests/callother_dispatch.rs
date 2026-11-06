use strider_target::ArchPreset;
use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass, classify};

fn expect_call(c: Option<CallOtherClass>) -> CallOtherAbi {
    match c {
        Some(CallOtherClass::Call(abi)) => abi,
        other => panic!("expected Call(_), got {other:?}"),
    }
}

/// An unknown user-op name classifies as `None` under EVERY preset, never a
/// silent default ABI.  Missing table entries are intentional, added on demand
/// when real binaries surface them; the lifter turns `None` into an
/// "unknown CallOther user-op" lift error downstream.
#[test]
fn unknown_name_returns_none_on_every_preset() {
    for &preset in ArchPreset::ALL {
        assert_eq!(
            classify(preset, "this_op_definitely_does_not_exist"),
            None,
            "{preset:?}: unknown name must fall through to None",
        );
    }
}

/// The arch-independent rows resolve identically under every preset that no
/// arch-specific row shadows, one memory-clobbering `Call` and one plain one.
#[test]
fn arch_independent_rows_resolve_on_every_preset() {
    for &preset in ArchPreset::ALL {
        let setend = expect_call(classify(preset, "setEndianState"));
        assert!(
            setend.clobbers_memory,
            "{preset:?}/setEndianState must break the memory edge",
        );
        let yield_op = expect_call(classify(preset, "Yield"));
        assert!(!yield_op.no_return, "{preset:?}/Yield");
        assert!(!yield_op.clobbers_memory, "{preset:?}/Yield");
    }
}

/// Rows a misclassification would miscompile, asserted by intent: each
/// returns (a NO_RETURN conditional trap drops the code after it), clobbers
/// memory (the vsx forms' access is missing from their p-code) and writes at
/// least the listed registers, resolving on the preset's own table.
#[test]
fn soundness_critical_rows_keep_their_footprint() {
    use ArchPreset::{Arm, ArmThumb, MipsBe32, MipsBe64, Ppc32Be, Ppc64Be};
    let mips_syscall: &[&str] = &["v0", "a3", "at", "hi", "lo"];
    let smccc: &[&str] = &["r0", "r1", "r2", "r3"];
    let rows: &[(ArchPreset, &str, &[&str])] = &[
        (Ppc32Be, "trapWord", &[]),
        (Ppc64Be, "trapWord", &[]),
        (Ppc64Be, "trapDoubleWord", &[]),
        (Ppc64Be, "trapDoubleWordImmediate", &[]),
        (Ppc64Be, "vsx300_15", &[]),
        (Ppc64Be, "vsx300_20", &[]),
        (Ppc64Be, "vsx300_22", &[]),
        (Ppc64Be, "vsx300_9", &[]),
        (Ppc64Be, "vsx207_1", &[]),
        (Arm, "software_smc", smccc),
        (ArmThumb, "software_hvc", smccc),
        (MipsBe32, "syscall", mips_syscall),
        (MipsBe64, "syscall", mips_syscall),
    ];
    for &(preset, name, writes) in rows {
        let abi = expect_call(classify(preset, name));
        assert!(!abi.no_return, "{preset:?}/{name} returns");
        assert!(abi.clobbers_memory, "{preset:?}/{name} clobbers memory");
        for r in writes {
            assert!(
                abi.implicit_writes.contains(r),
                "{preset:?}/{name} writes {r}"
            );
        }
        abi.build(&regs_for(preset))
            .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e}"));
    }
}

/// The x86 ops that reach memory without p-code saying so: MOVDIR64B's
/// 64-byte destination store and XSHA256's [ESI]/[EDI] streaming.
#[test]
fn x86_implicit_memory_ops_clobber_memory() {
    for name in ["movdir64b", "xsha256"] {
        let abi = expect_call(classify(ArchPreset::X86_64, name));
        assert!(abi.clobbers_memory, "{name}: access is implicit in the sla");
        assert!(abi.implicit_reads.is_empty(), "{name}");
        assert!(abi.implicit_writes.is_empty(), "{name}");
        assert_eq!(classify(ArchPreset::Aarch64, name), None, "aarch64/{name}");
    }
}

/// The `vp*` / `*_avx*` prefixes are NOT homogeneous, so no prefix family may
/// exist for them: gather / scatter / compress / maskmov / MXCSR ops share
/// those prefixes and reach memory the p-code does not model.  They must stay
/// unclassified rather than inherit `PURE` from a neighbour.
#[test]
fn avx_memory_ops_are_not_swallowed_by_a_prefix_family() {
    for name in [
        "vpgatherdd_avx512f",
        "vpscatterqq_avx512f",
        "vpcompressd_avx512f",
        "vpexpandd_avx512f",
        "vpmaskmovd_avx2",
        "vmaskmovps_avx",
        "vldmxcsr_avx",
        "vstmxcsr_avx",
    ] {
        for preset in [ArchPreset::X86, ArchPreset::X86_64] {
            assert_eq!(classify(preset, name), None, "{preset:?}/{name}");
        }
    }
}

/// `a64_TBL` is aarch64 table lookup across vector registers.
#[test]
fn aarch64_tbl_is_pure_and_scoped() {
    for preset in [ArchPreset::Aarch64, ArchPreset::Aarch64Be] {
        let abi = expect_call(classify(preset, "a64_TBL"));
        assert!(!abi.clobbers_memory, "{preset:?}");
    }
    assert_eq!(classify(ArchPreset::X86_64, "a64_TBL"), None);
    assert_eq!(classify(ArchPreset::Arm, "a64_TBL"), None);
}

/// ARM-32 NEON names outside the `Vector*` / `Float*` groups; saturation
/// sets FPSCR.QC.
#[test]
fn arm32_neon_scalar_ops_are_pure_and_scoped() {
    for (name, fpscr) in [
        ("SHA256ScheduleUpdate0", false),
        ("SHA256ScheduleUpdate1", false),
        ("SatQ", true),
        ("SignedSatQ", true),
        ("vrev", false),
    ] {
        let footprint: &[&str] = if fpscr { &["fpscr"] } else { &[] };
        for preset in [
            ArchPreset::Arm,
            ArchPreset::ArmBe,
            ArchPreset::ArmBeKernel,
            ArchPreset::ArmThumb,
        ] {
            let abi = expect_call(classify(preset, name));
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert_eq!(abi.implicit_reads, footprint, "{preset:?}/{name}");
            assert_eq!(abi.implicit_writes, footprint, "{preset:?}/{name}");
        }
        // `vrev` and `SatQ` are generic spellings; keep them off other arches.
        assert_eq!(classify(ArchPreset::X86_64, name), None, "x86/{name}");
        assert_eq!(classify(ArchPreset::Aarch64, name), None, "aarch64/{name}");
    }
}

/// Virtualisation, privileged, system, and shadow-stack user-ops the same
/// sweep surfaced stay UNCLASSIFIED on purpose: each needs a real register
/// footprint or memory effect, and a guessed `PURE` would be a miscompile.
/// Pinned so they are not swept in by a later prefix family.
#[test]
fn privileged_and_virtualisation_ops_stay_unclassified() {
    for (preset, name) in [
        (ArchPreset::X86_64, "vmmcall"),
        (ArchPreset::X86_64, "vmcall"),
        (ArchPreset::X86_64, "vmxoff"),
        (ArchPreset::X86_64, "encls_ecreate"),
        (ArchPreset::X86_64, "wrpkru"),
        (ArchPreset::X86_64, "stgi"),
        (ArchPreset::X86_64, "writeToUserShadowStack"),
        (ArchPreset::X86_64, "SegmentLimit"),
        (ArchPreset::X86_64, "xstore_available"),
        (ArchPreset::Aarch64, "SysOp_R"),
        (ArchPreset::Aarch64, "HaltBreakPoint"),
        (ArchPreset::Aarch64, "SVE_rdvl"),
        (ArchPreset::Aarch64, "SVE_pfalse"),
        (ArchPreset::MipsBe32, "move_to_thread_gpr"),
    ] {
        assert_eq!(classify(preset, name), None, "{preset:?}/{name}");
    }
}

const ARM_MODE_SWITCH_OPS: [&str; 10] = [
    "setAbortMode",
    "setFIQMode",
    "setIRQMode",
    "setMonitorMode",
    "setProcessorMode",
    "setStackMode",
    "setSupervisorMode",
    "setSystemMode",
    "setUndefinedMode",
    "setUserMode",
];

/// A row naming a register resolves only on the arch whose Sleigh table holds
/// that name, so the ARM processor-mode rows have to be checked against ARM's.
#[test]
fn arm_mode_switch_rows_resolve_their_banked_registers() {
    for preset in [
        ArchPreset::Arm,
        ArchPreset::ArmBe,
        ArchPreset::ArmBeKernel,
        ArchPreset::ArmThumb,
    ] {
        let regs = regs_for(preset);
        for name in ARM_MODE_SWITCH_OPS {
            let abi = classify(preset, name).unwrap_or_else(|| panic!("{preset:?}/{name}"));
            let built = strider_target::call_other_abi::CallOtherLookup::Class(abi)
                .built(&regs)
                .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e}"))
                .expect("a Call class resolves to a footprint");
            assert!(
                !built.implicit_writes.is_empty(),
                "{preset:?}/{name} must re-bank at least the stack pointer"
            );
        }
    }
}

/// FIQ banks R8-R12 on top of the R13, R14 and SPSR every exception mode
/// banks (ARM ARM DDI 0406C B1.3.2).  `ARM.sinc` models no banking at all, so
/// an ARM-32 processor-mode switch that named only `sp`/`lr` would let a read
/// of `r8` after `cps #0x11` forward the pre-switch value.  The wide set
/// applies to every mode op because the switch OUT of FIQ un-banks the same
/// registers and the incoming mode is unknown; `setStackMode` is the Thumb
/// MSP/PSP select, which re-banks `sp` alone.
#[test]
fn arm_mode_switch_writes_the_fiq_banked_registers() {
    for preset in ARM32_PRESETS {
        for name in ARM_MODE_SWITCH_OPS {
            let abi = expect_call(classify(preset, name));
            if name == "setStackMode" {
                assert_eq!(abi.implicit_writes, &["sp"], "{preset:?}/{name}");
                continue;
            }
            assert_eq!(
                abi.implicit_writes,
                &["r8", "r9", "r10", "r11", "r12", "sp", "lr", "spsr"],
                "{preset:?}/{name}"
            );
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert!(!abi.no_return, "{preset:?}/{name}");
            // Every name has to resolve on this preset's own register table.
            abi.build(&regs_for(preset))
                .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e}"));
        }
    }
}

fn regs_for(preset: ArchPreset) -> rsleigh::SleighRegs {
    preset.arch().probe_regs()
}

const ARM32_PRESETS: [ArchPreset; 4] = [
    ArchPreset::Arm,
    ArchPreset::ArmBe,
    ArchPreset::ArmBeKernel,
    ArchPreset::ArmThumb,
];

/// `cpsie a` / `cpsid a` are adjacent alternatives of one sub-constructor
/// (`ARMinstructions.sinc:1653-1654`), so classifying one and not the other
/// lifts the enable and fails the disable.  Same for the six `is*` privileged
/// state queries, which `ARMTHUMBinstructions.sinc` reads through `mrs`.
#[test]
fn arm_privileged_state_ops_are_classified_in_pairs() {
    for name in [
        "disableDataAbortInterrupts",
        "enableDataAbortInterrupts",
        "disableFIQinterrupts",
        "enableFIQinterrupts",
        "disableIRQinterrupts",
        "enableIRQinterrupts",
        "isFIQinterruptsEnabled",
        "isIRQinterruptsEnabled",
        "isCurrentModePrivileged",
        "isThreadMode",
    ] {
        for preset in ARM32_PRESETS {
            let abi = expect_call(classify(preset, name));
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert!(abi.implicit_writes.is_empty(), "{preset:?}/{name}");
        }
    }
}

/// The x86 rows whose ABI is an empty register footprint carry no information
/// beyond `PURE` / `MEM_CLOBBER`; arch scoping lives in the row's
/// `scope`, not in its class.  Pins the classification each name
/// resolves to on both x86 widths, and that no other arch answers for them.
#[test]
fn x86_empty_footprint_rows_are_pure_or_mem_clobber() {
    let pure = ["rdtsc", "rdtscp", "rdmsr", "readfsbase", "readgsbase"];
    let mem_clobber = ["swi"];
    for preset in [ArchPreset::X86, ArchPreset::X86_64] {
        for name in pure {
            assert_eq!(
                classify(preset, name),
                Some(CallOtherClass::PURE),
                "{preset:?}/{name}"
            );
        }
        for name in mem_clobber {
            assert_eq!(
                classify(preset, name),
                Some(CallOtherClass::MEM_CLOBBER),
                "{preset:?}/{name}"
            );
        }
    }
    for name in pure.iter().chain(&mem_clobber).chain(&["wrmsr"]) {
        for preset in [ArchPreset::Arm, ArchPreset::Aarch64, ArchPreset::MipsBe32] {
            assert_eq!(classify(preset, name), None, "{preset:?}/{name}");
        }
    }
}

/// Every name in the FXSAVE / XSAVE footprint resolves on both x86 widths.
/// An unresolvable name is a hard error at lift time, and the sla declares
/// `XMM8`-`XMM15` in the block it shares between the two widths, so the one
/// list has to work for both.
#[test]
fn the_x86_extended_state_footprint_resolves_on_both_widths() {
    for preset in [ArchPreset::X86, ArchPreset::X86_64] {
        let regs = regs_for(preset);
        for name in ["_fxrstor", "xrstor64", "_fxsave", "xsaves"] {
            let abi = expect_call(classify(preset, name));
            let built = abi
                .build(&regs)
                .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e:?}"));
            let moved = if name.contains("rstor") {
                built.implicit_writes.len()
            } else {
                built.implicit_reads.len()
            };
            // 8 ST + 32 XMM + 32 YMM_H + 8 K + 4 BND + BNDCFGU + BNDSTATUS
            // + 3 IA32_PL*_SSP + MXCSR + 8 FPU words. An inequality here
            // admitted dropping the whole AVX half, which is the stale
            // upper-lane read this row exists to model.
            assert_eq!(
                moved, 98,
                "{preset:?}/{name}: moves {moved} registers, expected the \
                 x87 + SSE + AVX + AVX-512 state"
            );
            // Identity, not just arity: `regs_to_vns` is a 1:1 map with no
            // dedup, so a name repeated in the list keeps the count while
            // silently dropping a register from the footprint.
            let vns = if name.contains("rstor") {
                &built.implicit_writes
            } else {
                &built.implicit_reads
            };
            let distinct: rustc_hash::FxHashSet<_> = vns.iter().collect();
            assert_eq!(
                distinct.len(),
                98,
                "{preset:?}/{name}: {} of 98 names resolve to distinct \
                 varnodes, so one is duplicated",
                distinct.len()
            );
        }
    }
}

/// A restore writes the state; a save only reads it. A restore that declared
/// no writes left the incoming value of every one of these live across it.
#[test]
fn a_restore_writes_the_state_a_save_reads_it() {
    let regs = regs_for(ArchPreset::X86_64);
    let restore = expect_call(classify(ArchPreset::X86_64, "_fxrstor"))
        .build(&regs)
        .expect("fxrstor builds");
    assert!(
        !restore.implicit_writes.is_empty(),
        "fxrstor writes nothing"
    );

    let save = expect_call(classify(ArchPreset::X86_64, "_fxsave"))
        .build(&regs)
        .expect("fxsave builds");
    assert!(save.implicit_writes.is_empty(), "fxsave writes a register");
    assert!(!save.implicit_reads.is_empty(), "fxsave reads nothing");
}

/// `slbfee.` and `icswx.` are `Rc=1` forms the Power ISA defines as setting
/// CR0, and the sla emits each as a pcodeop with no `cr0flags(...)`
/// (`ppc_isa.sinc:1613`, `ppc_a2.sinc:52`), so the table is the only model of
/// it. `stwcx.` is the contrast: its sla body does emit `cr0flags`, which is
/// why it correctly declares nothing here.
#[test]
fn ppc_dot_forms_the_sla_leaves_unflagged_declare_cr0() {
    for preset in [
        ArchPreset::Ppc32Be,
        ArchPreset::Ppc32Le,
        ArchPreset::Ppc64Be,
        ArchPreset::Ppc64Le,
    ] {
        let regs = regs_for(preset);
        for name in ["slbfeeDotOp", "icswxDotOp"] {
            let built = expect_call(classify(preset, name))
                .build(&regs)
                .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e:?}"));
            assert_eq!(
                built.implicit_writes,
                vec![regs.name_to_vn("cr0").expect("cr0 is a PowerPC register")],
                "{preset:?}/{name} writes CR0 and nothing else modellable"
            );
        }
        let stwcx = expect_call(classify(preset, "storeWordConditionalIndexed"))
            .build(&regs)
            .expect("stwcx builds");
        assert!(
            stwcx.implicit_writes.is_empty(),
            "{preset:?}: stwcx.'s sla body sets CR0 itself"
        );
    }
}

/// Every name in the PowerPC FPSCR / MSR footprints resolves on each PPC
/// preset. `CallOtherAbi::build` short-circuits on the first unresolvable
/// name, so one misspelling turns every function containing the instruction
/// into a hard lift failure, which nothing else here would catch.
///
/// `MoveToFPSCRField*` reaches only the ppc64 presets (`ppc_isa.sinc`, which
/// only the `ppc_64_isa*` slaspecs include), but the register NAMES come from
/// `ppc_common.sinc` and resolve on all four, which is what this pins.
#[test]
fn ppc_fpscr_and_msr_footprints_resolve_on_every_ppc_preset() {
    for preset in [
        ArchPreset::Ppc32Be,
        ArchPreset::Ppc32Le,
        ArchPreset::Ppc64Be,
        ArchPreset::Ppc64Le,
    ] {
        let regs = regs_for(preset);
        for name in [
            "MoveToFPSCRFields",
            "MoveToFPSCRFields1",
            "MoveToFPSCRFieldImmediate",
            "MoveToFPSCRFieldImmediate1",
            "WriteExternalEnable",
            "WriteExternalEnableImmediate",
        ] {
            let built = expect_call(classify(preset, name))
                .build(&regs)
                .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e:?}"));
            assert!(
                !built.implicit_writes.is_empty(),
                "{preset:?}/{name} models a register write"
            );
            let distinct: rustc_hash::FxHashSet<_> = built.implicit_writes.iter().collect();
            assert_eq!(
                distinct.len(),
                built.implicit_writes.len(),
                "{preset:?}/{name}: a name is duplicated in the footprint"
            );
        }
    }
}

/// Every AltiVec op a record form (`vcmp*.`, `bcd*.`) lifts to. The sla
/// writes no CR6 for any of them, so the table must. The `vectorCompare*`
/// names are shared with the non-record form.
const PPC_CR6_OPS: [&str; 38] = [
    "vectorCompareBoundsFloatingPoint",
    "vectorCompareEqualToFloatingPoint",
    "vectorCompareEqualToUnsignedByte",
    "vectorCompareEqualToUnsignedHalfWord",
    "vectorCompareEqualToUnsignedWord",
    "vectorCompareGreaterThanOrEqualToFloatingPoint",
    "vectorCompareGreaterThanFloatingPoint",
    "vectorCompareGreaterThanSignedByte",
    "vectorCompareGreaterThanConditionRegisterSignedHalfWord",
    "vectorCompareGreaterThanSignedWord",
    "vectorCompareGreaterThanUnsignedByte",
    "vectorCompareGreaterThanUnsignedHalfWord",
    "vectorCompareGreaterThanUnsignedWord",
    "altv207_14",
    "altv207_16",
    "altv207_18",
    "altv207_64",
    "altv207_65",
    "altv300_1",
    "altv300_2",
    "altv300_3",
    "altv300_4",
    "altv300_5",
    "altv300_6",
    "altv300_7",
    "altv300_8",
    "altv300_9",
    "altv300_10",
    "altv300_12",
    "altv300_13",
    "altv300_14",
    "altv300_21",
    "altv300_23",
    "altv300_25",
    "altv300_27",
    "altv300_29",
    "altv300_31",
    "vsx300_82",
];

const PPC_PRESETS: [ArchPreset; 4] = [
    ArchPreset::Ppc32Be,
    ArchPreset::Ppc32Le,
    ArchPreset::Ppc64Be,
    ArchPreset::Ppc64Le,
];

#[test]
fn ppc_record_form_vector_ops_write_cr6() {
    for preset in PPC_PRESETS {
        for name in PPC_CR6_OPS {
            let abi = expect_call(classify(preset, name));
            assert!(
                abi.implicit_writes.contains(&"cr6"),
                "{preset:?}/{name}: {abi:?}"
            );
        }
    }
}

/// The op `vcmpequb. v6,v0,v18` decodes to is one the table says writes CR6.
#[test]
fn vcmpequb_dot_decodes_to_a_cr6_write() {
    const WORD: u32 = 0x10C0_9406;
    for preset in [ArchPreset::Ppc64Le, ArchPreset::Ppc32Be] {
        let arch = preset.arch();
        let bytes = match arch.endianness() {
            strider_target::Endianness::Big => WORD.to_be_bytes(),
            strider_target::Endianness::Little => WORD.to_le_bytes(),
        };
        let reader = rsleigh::mem_readers::BufMemReader::new(bytes.to_vec(), 0x1000);
        let lift = arch.sleigh(reader).unwrap().lift_one(0x1000).unwrap();
        let names = &arch.decode_tables().user_op_names;
        let ops: Vec<&str> = lift
            .insns
            .iter()
            .filter(|i| i.opcode == rsleigh::Opcode::CallOther)
            .map(|i| names[usize::try_from(i.inputs[0].addr_off).unwrap()].as_str())
            .collect();
        assert_eq!(ops, ["vectorCompareEqualToUnsignedByte"], "{preset:?}");
        let abi = expect_call(classify(preset, ops[0]));
        assert_eq!(abi.implicit_writes, &["cr6"], "{preset:?}");
    }
}

/// `mtc0` passes its CP0 destination as an operand (`setCopReg(0, RD0, rt,
/// sel)`) while `mfc0` reads the register directly, so the op writes the whole
/// CP0 file: all eight selects of all 32 registers.
#[test]
fn mips_set_cop_reg_writes_every_cp0_register() {
    for preset in [
        ArchPreset::MipsBe32,
        ArchPreset::MipsLe32,
        ArchPreset::MipsBe64,
        ArchPreset::MipsLe64,
    ] {
        let abi = expect_call(classify(preset, "setCopReg"));
        assert!(abi.clobbers_memory, "{preset:?}");
        let built = abi.build(&regs_for(preset)).expect("every name resolves");
        let mut offs: Vec<u64> = built.implicit_writes.iter().map(|vn| vn.addr_off).collect();
        offs.sort_unstable();
        offs.dedup();
        let size = u64::from(built.implicit_writes[0].size);
        let want: Vec<u64> = (0..8u64)
            .flat_map(|sel| (0..32u64).map(move |r| 0x2000 + sel * 0x100 + r * size))
            .collect();
        assert_eq!(offs, want, "{preset:?}");
    }
}
