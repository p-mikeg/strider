#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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
    // x86's `swi` (INT) is a sound stub: no register reads or writes, just the
    // memory edge.  Pinned so an attempt to harmonize the two arms surfaces.
    let x86 = expect_call(classify(ArchPreset::X86_64, "swi"));
    assert_ne!(arm.implicit_reads, x86.implicit_reads);
    assert!(x86.implicit_reads.is_empty());
    assert!(x86.implicit_writes.is_empty());
    assert!(x86.clobbers_memory);
}

#[test]
fn arch_independent_mfence_agrees_across_presets() {
    let x86 = classify(ArchPreset::X86_64, "mfence");
    let arm = classify(ArchPreset::Arm, "mfence");
    let aarch = classify(ArchPreset::Aarch64, "mfence");
    // mfence is a fence, so the arch-independent table wins.
    assert!(matches!(x86, Some(CallOtherClass::Call(_))));
    assert_eq!(x86, arm);
    assert_eq!(x86, aarch);
}

#[test]
fn unknown_call_other_returns_none() {
    assert!(classify(ArchPreset::X86_64, "this_op_definitely_does_not_exist").is_none());
}

fn all_presets() -> [ArchPreset; 15] {
    [
        ArchPreset::X86,
        ArchPreset::X86_64,
        ArchPreset::Arm,
        ArchPreset::ArmBe,
        ArchPreset::ArmThumb,
        ArchPreset::Aarch64,
        ArchPreset::Aarch64Be,
        ArchPreset::MipsBe32,
        ArchPreset::MipsLe32,
        ArchPreset::MipsBe64,
        ArchPreset::MipsLe64,
        ArchPreset::Ppc32Be,
        ArchPreset::Ppc32Le,
        ArchPreset::Ppc64Be,
        ArchPreset::Ppc64Le,
    ]
}

/// An unknown user-op name classifies as `None` under EVERY preset, never a
/// silent default ABI.  Missing table entries are intentional, added on demand
/// when real binaries surface them; the lifter turns `None` into
/// `UnknownCallOtherError` downstream.
#[test]
fn unknown_name_returns_none_on_every_preset() {
    for preset in all_presets() {
        assert_eq!(
            classify(preset, "this_op_definitely_does_not_exist"),
            None,
            "{preset:?}: unknown name must fall through to None",
        );
    }
}

/// The arch-independent `NoOp` rows (Sleigh decoder context:
/// `setEndianState` / `setISAMode`) and `NoReturn` row (`trap`) resolve
/// identically under every preset, since no arch-specific row shadows those
/// names.  The arch-specific NoReturn ops are x86's `sysret` / `sysexit`,
/// pinned by the unit tests' `sysret_and_swapgs_are_x86_only`.
#[test]
fn arch_independent_noop_and_noreturn_resolve_on_every_preset() {
    for preset in all_presets() {
        for n in ["setEndianState", "setISAMode"] {
            assert_eq!(
                classify(preset, n),
                Some(CallOtherClass::NoOp),
                "{preset:?}/{n}",
            );
        }
        assert_eq!(
            classify(preset, "trap"),
            Some(CallOtherClass::NO_RETURN),
            "{preset:?}/trap",
        );
    }
}
