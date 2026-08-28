use strider_target::ArchPreset;
use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass, classify};

fn expect_call(c: Option<CallOtherClass>) -> CallOtherAbi {
    match c {
        Some(CallOtherClass::Call(abi)) => abi,
        other => panic!("expected Call(_), got {other:?}"),
    }
}

#[test]
fn arm_software_interrupt_reads_r7_and_r0_through_r6() {
    // ARM emits `software_interrupt` (not `swi`) for SVC.
    let abi = expect_call(classify(ArchPreset::Arm, "software_interrupt"));
    assert_eq!(
        abi.implicit_reads,
        &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"]
    );
    assert_eq!(abi.implicit_writes, &["r0"]);
    assert!(abi.clobbers_memory);
}

#[test]
fn arm_be_and_thumb_share_software_interrupt_with_arm() {
    let arm = expect_call(classify(ArchPreset::Arm, "software_interrupt"));
    let arm_be = expect_call(classify(ArchPreset::ArmBe, "software_interrupt"));
    let arm_thumb = expect_call(classify(ArchPreset::ArmThumb, "software_interrupt"));
    assert_eq!(arm.implicit_reads, arm_be.implicit_reads);
    assert_eq!(arm.implicit_reads, arm_thumb.implicit_reads);
    assert_eq!(arm.implicit_writes, arm_be.implicit_writes);
    assert_eq!(arm.implicit_writes, arm_thumb.implicit_writes);
}

#[test]
fn x86_swi_differs_from_arm_software_interrupt() {
    let arm = expect_call(classify(ArchPreset::Arm, "software_interrupt"));
    // x86's `swi` (INT) is a sound stub: empty register channels, memory edge
    // only.  Pinned so an attempt to harmonize the two arms surfaces.
    let x86 = expect_call(classify(ArchPreset::X86_64, "swi"));
    assert_ne!(arm.implicit_reads, x86.implicit_reads);
    assert!(x86.implicit_reads.is_empty());
    assert!(x86.implicit_writes.is_empty());
    assert!(x86.clobbers_memory);
}

#[test]
fn arch_independent_barrier_agrees_across_presets() {
    let x86 = classify(ArchPreset::X86_64, "DataMemoryBarrier");
    let arm = classify(ArchPreset::Arm, "DataMemoryBarrier");
    let aarch = classify(ArchPreset::Aarch64, "DataMemoryBarrier");
    // A barrier row lives in the arch-independent table, so every preset sees
    // the same classification.
    assert!(matches!(x86, Some(CallOtherClass::Call(_))));
    assert_eq!(x86, arm);
    assert_eq!(x86, aarch);
}

#[test]
fn unknown_call_other_returns_none() {
    assert!(classify(ArchPreset::X86_64, "this_op_definitely_does_not_exist").is_none());
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
        // `setISAMode` is covered by `set_isa_mode_is_noop_on_every_arch`.
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

/// x86 SIMD / crypto / bit-manipulation user-ops whose sla constructors list
/// every operand: pure register compute, no memory edge, no implicit channel.
#[test]
fn x86_simd_crypto_ops_are_pure_with_empty_channels() {
    for name in [
        "aesdec",
        "aesdeclast",
        "aesenc",
        "aesenclast",
        "aesimc",
        "aeskeygenassist",
        "crc32",
        "movntdqa",
        "pblendvb",
        "psraw",
        "sha1msg1_sha",
        "sha1msg2_sha",
        "sha1nexte_sha",
        "sha1rnds4_sha",
        "vbroadcasti32x4_avx512f",
        "vmovdqa64_avx512f",
        "vmovntdq_avx",
        "vpaddd_avx2",
        "vpaddq_avx",
        "vpblendd_avx2",
        "vpbroadcastb_avx512bw",
        "vpcmpgtb_avx2",
        "vpermd_avx2",
        "vpshufb_avx",
        "vpshufb_avx2",
        "vpshufd_avx",
        "vpsraw_avx2",
        "vpsrldq_avx",
    ] {
        for preset in [ArchPreset::X86, ArchPreset::X86_64] {
            let abi = expect_call(classify(preset, name));
            assert!(abi.implicit_reads.is_empty(), "{preset:?}/{name}");
            assert!(abi.implicit_writes.is_empty(), "{preset:?}/{name}");
            assert!(!abi.clobbers_memory, "{preset:?}/{name}: must stay PURE");
            assert!(!abi.no_return, "{preset:?}/{name}");
        }
        // Scoped to x86, so another spec reusing the spelling cannot inherit it.
        assert_eq!(classify(ArchPreset::Aarch64, name), None, "aarch64/{name}");
        assert_eq!(classify(ArchPreset::Arm, name), None, "arm/{name}");
        assert_eq!(classify(ArchPreset::MipsBe32, name), None, "mips/{name}");
        assert_eq!(classify(ArchPreset::Ppc32Be, name), None, "ppc/{name}");
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

/// Every `NEON_*` user-op in the AArch64 sla is register compute, so the
/// prefix family covers them; NEON memory access lifts to real p-code
/// Load / Store, never to one of these.
#[test]
fn aarch64_neon_prefix_family_is_pure() {
    for name in [
        "NEON_aesd",
        "NEON_aese",
        "NEON_aesimc",
        "NEON_aesmc",
        "NEON_rev32",
        "NEON_sha256su0",
        "NEON_pmull",
        "NEON_this_one_does_not_exist_yet",
    ] {
        for preset in [ArchPreset::Aarch64, ArchPreset::Aarch64Be] {
            let abi = expect_call(classify(preset, name));
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert!(abi.implicit_reads.is_empty(), "{preset:?}/{name}");
            assert!(abi.implicit_writes.is_empty(), "{preset:?}/{name}");
        }
    }
    // The family is aarch64's; arm-32 spells its NEON ops `Vector*`.
    assert_eq!(classify(ArchPreset::Arm, "NEON_aese"), None);
    assert_eq!(classify(ArchPreset::MipsBe32, "NEON_aese"), None);
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

/// `SVE_ldr` / `SVE_str` take a BASE REGISTER, not a dynamic memory varnode,
/// so no p-code Load / Store is emitted and the access is implicit.  They are
/// the reason `SVE_` is not a prefix family: `SVE_fnmla` next to them is pure.
#[test]
fn sve_load_store_clobber_memory_unlike_sve_compute() {
    for name in ["SVE_ldr", "SVE_str"] {
        for preset in [ArchPreset::Aarch64, ArchPreset::Aarch64Be] {
            let abi = expect_call(classify(preset, name));
            assert!(
                abi.clobbers_memory,
                "{preset:?}/{name}: implicit memory access"
            );
        }
        assert_eq!(classify(ArchPreset::X86_64, name), None, "x86/{name}");
    }
    let compute = expect_call(classify(ArchPreset::Aarch64, "SVE_fnmla"));
    assert!(!compute.clobbers_memory, "SVE compute stays pure");
}

/// ARM-32 NEON names outside the `Vector*` / `Float*` groups.
#[test]
fn arm32_neon_scalar_ops_are_pure_and_scoped() {
    for name in [
        "SHA256ScheduleUpdate0",
        "SHA256ScheduleUpdate1",
        "SatQ",
        "SignedSatQ",
        "vrev",
    ] {
        for preset in [
            ArchPreset::Arm,
            ArchPreset::ArmBe,
            ArchPreset::ArmBeKernel,
            ArchPreset::ArmThumb,
        ] {
            let abi = expect_call(classify(preset, name));
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert!(abi.implicit_reads.is_empty(), "{preset:?}/{name}");
            assert!(abi.implicit_writes.is_empty(), "{preset:?}/{name}");
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

const ARM_MODE_SWITCH_OPS: [&str; 9] = [
    "setAbortMode",
    "setFIQMode",
    "setIRQMode",
    "setMonitorMode",
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

/// `ARM.sinc` and `ARMTHUMBinstructions.sinc` are the only vendored specs
/// declaring these pcodeops, and `sp` / `lr` are ARM-32 register names: off
/// ARM-32 the write list either fails to resolve or, worse, resolves against
/// an unrelated arch's `sp` and claims a stack pointer clobbered that the
/// instruction never touches.
#[test]
fn arm_mode_switch_rows_are_scoped_to_arm32() {
    for &preset in ArchPreset::ALL {
        if matches!(
            preset,
            ArchPreset::Arm | ArchPreset::ArmBe | ArchPreset::ArmBeKernel | ArchPreset::ArmThumb
        ) {
            continue;
        }
        for name in ARM_MODE_SWITCH_OPS {
            assert_eq!(classify(preset, name), None, "{preset:?}/{name}");
        }
    }
}

fn regs_for(preset: ArchPreset) -> rsleigh::SleighRegs {
    preset.arch().probe_regs().expect("probe_regs")
}

const ARM32_PRESETS: [ArchPreset; 4] = [
    ArchPreset::Arm,
    ArchPreset::ArmBe,
    ArchPreset::ArmBeKernel,
    ArchPreset::ArmThumb,
];

/// Every `Float*` user-op `ARMneon.sinc` declares, the float half of the NEON
/// group whose integer half is `Vector*`.  Each is used only as
/// `Xd = op(Xn, Xm, esize)` over listed registers, and `ARMneon.sinc` names no
/// `[ram]` address anywhere, so the whole family is `PURE`.  Unclassified,
/// every one of them is a hard lift error on any ARM function containing a
/// NEON float instruction.
const ARM32_FLOAT_NEON_OPS: [&str; 21] = [
    "FloatCompareGE",
    "FloatCompareGT",
    "FloatSingleToBFloat16",
    "FloatToSignedRound",
    "FloatToUnsignedRound",
    "FloatVectorAbsolute",
    "FloatVectorAbsoluteDifference",
    "FloatVectorAdd",
    "FloatVectorCompareEqual",
    "FloatVectorCompareGreaterThan",
    "FloatVectorCompareGreaterThanOrEqual",
    "FloatVectorMax",
    "FloatVectorMin",
    "FloatVectorMult",
    "FloatVectorMultiplyAccumulate",
    "FloatVectorMultiplySubtract",
    "FloatVectorNeg",
    "FloatVectorPairwiseAdd",
    "FloatVectorPairwiseMax",
    "FloatVectorPairwiseMin",
    "FloatVectorSub",
];

#[test]
fn arm32_float_neon_prefix_family_is_pure() {
    for name in ARM32_FLOAT_NEON_OPS {
        for preset in ARM32_PRESETS {
            let abi = expect_call(classify(preset, name));
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert!(abi.implicit_reads.is_empty(), "{preset:?}/{name}");
            assert!(abi.implicit_writes.is_empty(), "{preset:?}/{name}");
        }
        assert_eq!(classify(ArchPreset::Aarch64, name), None, "aarch64/{name}");
        assert_eq!(classify(ArchPreset::X86_64, name), None, "x86/{name}");
    }
    // PowerPC's `Scalar_SPFP.sinc` and x86's `ia.sinc` spell unrelated ops
    // `FloatingPoint*`, which share the prefix; the arch scoping is what keeps
    // them out.
    assert_eq!(classify(ArchPreset::Ppc32Be, "FloatingPointAdd"), None);
    assert_eq!(
        classify(ArchPreset::X86_64, "FloatingPointAbsoluteValue"),
        None
    );
}

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

/// PLD / PLDW / PLI all take the address as a VALUE (`addrmode2` exports
/// `rn + offset`), so none emits a p-code Load and all three are pure markers.
#[test]
fn arm_preload_hints_are_pure() {
    for name in [
        "HintPreloadData",
        "HintPreloadDataForWrite",
        "HintPreloadInstruction",
    ] {
        let abi = expect_call(classify(ArchPreset::Arm, name));
        assert!(!abi.clobbers_memory, "{name}");
        assert!(abi.implicit_reads.is_empty(), "{name}");
    }
}

/// CLREX is emitted as a bare `ClearExclusiveLocal();` -- no operands, no
/// output, no `[ram]` -- so it classifies like its exclusive-monitor siblings.
#[test]
fn arm_clrex_is_pure_like_its_monitor_siblings() {
    for name in [
        "ClearExclusiveLocal",
        "ExclusiveAccess",
        "ExclusiveMonitorPass",
        "ExclusiveMonitorsStatus",
        "hasExclusiveAccess",
    ] {
        for preset in ARM32_PRESETS {
            let abi = expect_call(classify(preset, name));
            assert!(!abi.clobbers_memory, "{preset:?}/{name}");
            assert!(abi.implicit_reads.is_empty(), "{preset:?}/{name}");
            assert!(abi.implicit_writes.is_empty(), "{preset:?}/{name}");
        }
    }
}

const MIPS_PRESETS: [ArchPreset; 4] = [
    ArchPreset::MipsBe32,
    ArchPreset::MipsLe32,
    ArchPreset::MipsBe64,
    ArchPreset::MipsLe64,
];

/// `mips.sinc` declares `syscall` and `mips32Instructions.sinc` emits it, so
/// without a MIPS-scoped row every MIPS function containing one fails to lift:
/// the x86_64 row is scoped to x86_64 and the PPC one to the PPC presets.
#[test]
fn mips_syscall_carries_the_linux_footprint() {
    for preset in MIPS_PRESETS {
        let abi = expect_call(classify(preset, "syscall"));
        assert!(
            abi.clobbers_memory,
            "{preset:?}: a kernel entry touches memory"
        );
        assert!(!abi.no_return, "{preset:?}: a syscall returns");
        // v0 carries the number in and the result out; a3 is the error flag.
        assert!(abi.implicit_reads.contains(&"v0"), "{preset:?}");
        assert!(abi.implicit_reads.contains(&"a0"), "{preset:?}");
        assert!(abi.implicit_writes.contains(&"v0"), "{preset:?}");
        assert!(abi.implicit_writes.contains(&"a3"), "{preset:?}");
        // Every name has to resolve on this preset's own register table.
        abi.build(&regs_for(preset))
            .unwrap_or_else(|e| panic!("{preset:?}: {e}"));
    }
    // n64 passes eight arguments; GHIDRA's MIPS spec spells a4..a7 as t0..t3.
    let n64 = expect_call(classify(ArchPreset::MipsBe64, "syscall"));
    assert!(n64.implicit_reads.contains(&"t3"));
    let o32 = expect_call(classify(ArchPreset::MipsBe32, "syscall"));
    assert!(!o32.implicit_reads.contains(&"t3"));
}
