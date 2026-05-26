//! Integration: `strider_target::call_other_abi::classify` returns the
//! documented per-arch ABIs and falls through to the arch-independent
//! table for shared opcodes (`mfence`, `cpuid`, …).
//!
//! Pins the contract that:
//! - ARM `swi` reads `r7/r0..r6` and writes `r0` (Linux SVC).
//! - x86_64 `swi` does NOT use the same shape (it's a different ISA's
//!   software interrupt; the arch-specific arm models it as a stub).
//! - `mfence` resolves the same way under any preset (arch-independent).

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
fn arm_swi_reads_r7_and_r0_through_r6() {
    let abi = expect_call(classify(ArchPreset::Arm, "swi"));
    assert_eq!(
        abi.implicit_reads,
        &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"]
    );
    assert_eq!(abi.implicit_writes, &["r0"]);
    assert!(abi.clobbers_memory);
}

#[test]
fn arm_be_and_thumb_share_swi_with_arm() {
    let arm = expect_call(classify(ArchPreset::Arm, "swi"));
    let arm_be = expect_call(classify(ArchPreset::ArmBe, "swi"));
    let arm_thumb = expect_call(classify(ArchPreset::ArmThumb, "swi"));
    assert_eq!(arm.implicit_reads, arm_be.implicit_reads);
    assert_eq!(arm.implicit_reads, arm_thumb.implicit_reads);
    assert_eq!(arm.implicit_writes, arm_be.implicit_writes);
    assert_eq!(arm.implicit_writes, arm_thumb.implicit_writes);
}

#[test]
fn x86_64_swi_differs_from_arm_swi() {
    let arm = expect_call(classify(ArchPreset::Arm, "swi"));
    let x86 = expect_call(classify(ArchPreset::X86_64, "swi"));
    // x86 swi is a sound stub: no register reads/writes, just the
    // memory edge.  Pinning the divergence here so a future
    // attempt to "harmonize" the two arms surfaces explicitly.
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
    // mfence is a fence — the arch-independent table wins.
    assert!(matches!(x86, Some(CallOtherClass::Call(_))));
    assert_eq!(x86, arm);
    assert_eq!(x86, aarch);
}

#[test]
fn unknown_call_other_returns_none() {
    assert!(classify(ArchPreset::X86_64, "this_op_definitely_does_not_exist").is_none());
}
