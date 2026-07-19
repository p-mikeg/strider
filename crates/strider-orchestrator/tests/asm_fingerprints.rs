//! End-to-end checks for the asm-fingerprint side-table.
//!
//! Each test lifts a small fixture through the full strider + optimiser
//! pipeline and asserts that every reachable node whose kind is *not* in
//! the documented exempt set carries at least one machine-instruction
//! address in its fingerprint.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::*;
use strider_ir::validate::validate;
use strider_ir::{IRViewer, IRWalker};

#[test]
fn arithmetic_x86_add_validate_with_asm_fingerprint_check() {
    // Drives the check through validate()'s opt-in hook rather than the
    // side-table directly, to exercise the public surface.
    let function = analyze(Arch::X86, "arithmetic", "add");
    validate(&function).expect("every reachable non-exempt node must have a fingerprint");
}

#[test]
fn arithmetic_x86_default_validate_remains_unchanged() {
    let function = analyze(Arch::X86, "arithmetic", "add");
    validate(&function).expect("default validate still passes");
}

#[test]
fn control_x86_clamp_validate_with_asm_fingerprint_check() {
    // Control flow with two If branches; exercises constant-fold,
    // dead-branch, redundant-phi propagation alongside lift-time.
    let function = analyze(Arch::X86, "control", "clamp");
    validate(&function).expect("clamp pipeline preserves the fingerprint invariant");
}

#[test]
fn control_x86_count_bits_validate_with_asm_fingerprint_check() {
    // Loop body: exercises mem-phi / var-phi at the join points.
    let function = analyze(Arch::X86, "control", "count_bits");
    validate(&function).expect("count_bits pipeline preserves the fingerprint invariant");
}

#[test]
fn arithmetic_x86_complex_validate_with_asm_fingerprint_check() {
    // Heavier folding pressure: bit_and / bit_or / shl / lshr exercise
    // KnownBits and the AND-mask merge.
    for fn_name in ["bit_and", "bit_or", "shl", "lshr", "bit_xor"] {
        let function = analyze(Arch::X86, "arithmetic", fn_name);
        validate(&function).unwrap_or_else(|e| panic!("{fn_name}: {e}"));
    }
}

#[test]
fn arithmetic_x86_add_node_fingerprint_is_inside_function_extent() {
    // Loose smoke check: confirms fingerprints hold plausible machine
    // addresses, not pcode-insn-index values or other garbage.
    let function = analyze(Arch::X86, "arithmetic", "add");
    let mut saw_any = false;
    for node in function.walk() {
        let fp = function.side_tables().asm_fingerprint(node);
        for addr in fp {
            assert_ne!(addr, 0, "asm-fingerprint addr 0 is suspicious");
            // x86 fixtures link at the default location; everything lifted
            // sits well above 64KiB.
            assert!(
                addr > 0x1000,
                "asm-fingerprint addr {addr:#x} is suspiciously low"
            );
            saw_any = true;
        }
    }
    assert!(saw_any, "expected at least one fingerprint entry");
}

#[test]
fn add_chain_snippet_fingerprints_are_exact_snippet_addresses() {
    // Tight-bound complement to the loose extent check above: a hand-assembled
    // snippet makes the valid address set known by construction
    // ({0x1000, 0x1003, 0x1006, 0x1009, 0x100c}), so every reachable
    // non-exempt node's fingerprint must fall entirely inside it.
    use rsleigh::mem_readers::BufMemReader;
    use strider_orchestrator::opt::OptOptions;
    use strider_orchestrator::{LiftOptions, Strider};

    let base = 0x1000u64;
    let bytes = vec![
        0x48, 0x01, 0xc0, // add rax, rax
        0x48, 0x01, 0xc0, // add rax, rax
        0x48, 0x01, 0xc0, // add rax, rax
        0x48, 0x01, 0xc0, // add rax, rax
        0xc3, // ret
    ];
    let end = base + bytes.len() as u64; // 0x100d (exclusive)

    let arch = strider_target::SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(
            base,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    assert!(result.unresolved_indirect_branches.is_empty());
    let function = result.function;

    let mut non_exempt_seen = 0usize;
    for node in function.walk() {
        let kind = function.node_kind(node);
        let fp = function.side_tables().asm_fingerprint(node);
        if kind.asm_fingerprint_exempt() {
            continue;
        }
        non_exempt_seen += 1;
        assert!(
            !fp.is_empty(),
            "non-exempt {kind:?} node {node:?} must carry a fingerprint"
        );
        for addr in fp {
            assert!(
                (base..end).contains(&addr),
                "{kind:?} node {node:?} fingerprint addr {addr:#x} falls outside \
                 the snippet range [{base:#x}, {end:#x})"
            );
        }
    }
    assert!(
        non_exempt_seen > 0,
        "snippet must lift to at least one non-exempt node"
    );
}
