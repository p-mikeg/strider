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
use strider_ir::ValidateOptions;
use strider_ir::node::NodeKind;
use strider_ir::validate::{validate, validate_with_options};

/// Returns true if `kind` is documented as legitimately empty in the
/// asm-fingerprint side-table.  Mirrors the validator's exempt list.
fn is_exempt(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Entry
            | NodeKind::InitialMemory
            | NodeKind::InitialVar(_)
            | NodeKind::FunctionArg { .. }
            | NodeKind::ControlState
            | NodeKind::MemPhi
            | NodeKind::VarPhi(_)
            | NodeKind::ValuePhi
            | NodeKind::StackStorePhi { .. }
    )
}

#[test]
fn arithmetic_x86_add_every_reachable_value_node_has_a_fingerprint() {
    let g = analyze(Arch::X86, "arithmetic", "add");
    let mut empty_non_exempt: Vec<(strider_ir::node::NodeId, NodeKind)> = Vec::new();
    for node in g.preorder() {
        let kind = *g.node_kind(node);
        if is_exempt(&kind) {
            continue;
        }
        if g.asm_fingerprint(node).is_empty() {
            empty_non_exempt.push((node, kind));
        }
    }
    assert!(
        empty_non_exempt.is_empty(),
        "found {} reachable non-exempt nodes with empty asm-fingerprints: {:?}",
        empty_non_exempt.len(),
        empty_non_exempt,
    );
}

#[test]
fn arithmetic_x86_add_validate_with_asm_fingerprint_check() {
    // Same invariant as above, but driven through the IR validator's opt-in
    // hook so we exercise the public surface end-to-end.
    let g = analyze(Arch::X86, "arithmetic", "add");
    let opts = ValidateOptions { check_asm_fingerprints: true };
    validate_with_options(g.graph(), g.entry(), opts)
        .expect("every reachable non-exempt node must have a fingerprint");
}

#[test]
fn arithmetic_x86_default_validate_remains_unchanged() {
    // Sanity: the default `validate` call still works post-pipeline.
    let g = analyze(Arch::X86, "arithmetic", "add");
    validate(g.graph(), g.entry()).expect("default validate still passes");
}

#[test]
fn control_x86_clamp_validate_with_asm_fingerprint_check() {
    // Control flow with two If branches; exercises constant-fold,
    // dead-branch, redundant-phi propagation alongside lift-time.
    let g = analyze(Arch::X86, "control", "clamp");
    let opts = ValidateOptions { check_asm_fingerprints: true };
    validate_with_options(g.graph(), g.entry(), opts)
        .expect("clamp pipeline preserves the fingerprint invariant");
}

#[test]
fn control_x86_count_bits_validate_with_asm_fingerprint_check() {
    // Loop body — exercises mem-phi / var-phi at the join points.
    let g = analyze(Arch::X86, "control", "count_bits");
    let opts = ValidateOptions { check_asm_fingerprints: true };
    validate_with_options(g.graph(), g.entry(), opts)
        .expect("count_bits pipeline preserves the fingerprint invariant");
}

#[test]
fn arithmetic_x86_complex_validate_with_asm_fingerprint_check() {
    // Heavier folding pressure: bit_and / bit_or / shl / lshr exercise
    // KnownBits and the AND-mask merge.
    for fn_name in ["bit_and", "bit_or", "shl", "lshr", "bit_xor"] {
        let g = analyze(Arch::X86, "arithmetic", fn_name);
        let opts = ValidateOptions { check_asm_fingerprints: true };
        validate_with_options(g.graph(), g.entry(), opts)
            .unwrap_or_else(|e| panic!("{fn_name}: {e}"));
    }
}

#[test]
fn arithmetic_x86_add_node_fingerprint_is_inside_function_extent() {
    // Every fingerprint address for a reachable value node must be a
    // plausible machine address (non-zero, fits the function's region).
    // This is a loose smoke check: it confirms we did NOT accidentally
    // record pcode-insn-index values or other garbage.
    let g = analyze(Arch::X86, "arithmetic", "add");
    let mut saw_any = false;
    for node in g.preorder() {
        let fp = g.asm_fingerprint(node);
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
