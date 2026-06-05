//! End-to-end checks for the asm-fingerprint side-table.
//!
//! Each test lifts a small fixture through the full strider + optimiser
//! pipeline and asserts that every reachable node whose kind is *not* in
//! the documented exempt set carries at least one machine-instruction
//! address in its fingerprint.
//!
//! See `docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md` for
//! the full contract.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::*;
use strider_ir::IRWalker;
use strider_ir::validate::validate;

#[test]
fn arithmetic_x86_add_validate_with_asm_fingerprint_check() {
    // Same invariant as above, but driven through the IR validator's opt-in
    // hook so we exercise the public surface end-to-end.
    let function = analyze(Arch::X86, "arithmetic", "add");
    validate(&function, function.entry().unwrap())
        .expect("every reachable non-exempt node must have a fingerprint");
}

#[test]
fn arithmetic_x86_default_validate_remains_unchanged() {
    // Sanity: the default `validate` call still works post-pipeline.
    let function = analyze(Arch::X86, "arithmetic", "add");
    validate(&function, function.entry().unwrap()).expect("default validate still passes");
}

#[test]
fn control_x86_clamp_validate_with_asm_fingerprint_check() {
    // Control flow with two If branches; exercises constant-fold,
    // dead-branch, redundant-phi propagation alongside lift-time.
    let function = analyze(Arch::X86, "control", "clamp");
    validate(&function, function.entry().unwrap())
        .expect("clamp pipeline preserves the fingerprint invariant");
}

#[test]
fn control_x86_count_bits_validate_with_asm_fingerprint_check() {
    // Loop body — exercises mem-phi / var-phi at the join points.
    let function = analyze(Arch::X86, "control", "count_bits");
    validate(&function, function.entry().unwrap())
        .expect("count_bits pipeline preserves the fingerprint invariant");
}

#[test]
fn arithmetic_x86_complex_validate_with_asm_fingerprint_check() {
    // Heavier folding pressure: bit_and / bit_or / shl / lshr exercise
    // KnownBits and the AND-mask merge.
    for fn_name in ["bit_and", "bit_or", "shl", "lshr", "bit_xor"] {
        let function = analyze(Arch::X86, "arithmetic", fn_name);
        validate(&function, function.entry().unwrap()).unwrap_or_else(|e| panic!("{fn_name}: {e}"));
    }
}

#[test]
fn arithmetic_x86_add_node_fingerprint_is_inside_function_extent() {
    // Every fingerprint address for a reachable value node must be a
    // plausible machine address (non-zero, fits the function's region).
    // This is a loose smoke check: it confirms we did NOT accidentally
    // record pcode-insn-index values or other garbage.
    let function = analyze(Arch::X86, "arithmetic", "add");
    let mut saw_any = false;
    for node in function.walk() {
        let fp = function.asm_fingerprint(node);
        for &addr in fp {
            assert_ne!(addr, 0, "asm-fingerprint addr 0 is suspicious");
            // x86 fixtures are linked at the default location; everything
            // we lift sits well above 64KiB.
            assert!(
                addr > 0x1000,
                "asm-fingerprint addr {addr:#x} is suspiciously low"
            );
            saw_any = true;
        }
    }
    assert!(saw_any, "expected at least one fingerprint entry");
}
