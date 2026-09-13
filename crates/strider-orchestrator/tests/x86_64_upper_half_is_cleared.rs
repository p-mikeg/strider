//! A 32-bit destination clears the upper half of its 64-bit container.
//!
//! Intel SDM Vol. 2B: for `RDTSC` / `RDPMC`, "in 64-bit mode, the high-order 32
//! bits of each of RAX and RDX are cleared". The sla reaches that through
//! `check_EAX_dest` / `check_EDX_dest`; three constructors wrote the halves
//! from a temporary without them, so the container kept the CALLER's bits and
//! the result also carried a false dependency on the incoming register.

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::OptOptions;

mod common;

const BASE: u64 = 0x10000;
/// The mask a read-modify-write of the low half leaves behind.
const STALE_UPPER_HALF: u128 = 0xFFFF_FFFF_0000_0000;

fn keeps_the_callers_high_half(bytes: Vec<u8>) -> bool {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let result = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    let f = &result.function;
    f.graph()
        .all_value_ids()
        .any(|v| f.int_const_u128(v).is_some_and(|c| c == STALE_UPPER_HALF))
}

#[test]
fn reading_a_counter_into_eax_edx_clears_the_upper_halves() {
    for (name, bytes) in [
        ("rdtsc", vec![0x0f, 0x31, 0xc3]),
        ("rdpmc", vec![0x0f, 0x33, 0xc3]),
        ("xgetbv", vec![0x0f, 0x01, 0xd0, 0xc3]),
    ] {
        assert!(
            !keeps_the_callers_high_half(bytes),
            "{name} kept the caller's high 32 bits in RAX/RDX",
        );
    }
}

/// `rax` after `mov rdi, -1; mov rsi, -1; cld; <insn>; mov rax, <reg>; ret`.
fn register_after(insn: &[u8], reg: Reg) -> Option<u128> {
    let mut bytes = vec![0x48, 0xc7, 0xc7, 0xff, 0xff, 0xff, 0xff];
    bytes.extend([0x48, 0xc7, 0xc6, 0xff, 0xff, 0xff, 0xff]);
    bytes.push(0xfc);
    bytes.extend(insn);
    bytes.extend(match reg {
        Reg::Rsi => [0x48, 0x89, 0xf0],
        Reg::Rdi => [0x48, 0x89, 0xf8],
    });
    bytes.push(0xc3);
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let f = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.int_const_u128(f.node_inputs(ret)[2])
}

#[derive(Debug, Clone, Copy)]
enum Reg {
    Rsi,
    Rdi,
}

/// With a 0x67 prefix a string instruction steps ESI/EDI, clearing the upper
/// halves of RSI/RDI.
#[test]
fn an_address_size_override_clears_the_upper_half_of_the_string_registers() {
    let cases = [
        ("stosb", &[0x67, 0xaa][..], Reg::Rdi, 0),
        ("stosq", &[0x67, 0x48, 0xab], Reg::Rdi, 7),
        ("lodsb", &[0x67, 0xac], Reg::Rsi, 0),
        ("lodsd", &[0x67, 0xad], Reg::Rsi, 3),
        ("movsb", &[0x67, 0xa4], Reg::Rsi, 0),
        ("movsb", &[0x67, 0xa4], Reg::Rdi, 0),
        ("movsq", &[0x67, 0x48, 0xa5], Reg::Rsi, 7),
        ("movsq", &[0x67, 0x48, 0xa5], Reg::Rdi, 7),
        ("cmpsb", &[0x67, 0xa6], Reg::Rsi, 0),
        ("cmpsb", &[0x67, 0xa6], Reg::Rdi, 0),
        ("scasb", &[0x67, 0xae], Reg::Rdi, 0),
        ("insb", &[0x67, 0x6c], Reg::Rdi, 0),
        ("outsb", &[0x67, 0x6e], Reg::Rsi, 0),
    ];
    let mut wrong = Vec::new();
    for (name, insn, reg, value) in cases {
        let got = register_after(insn, reg);
        if got != Some(value) {
            wrong.push(format!("0x67 {name} {reg:?}: {got:x?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// With a 0x67 prefix a `rep` counts in ECX, clearing the upper half of RCX.
#[test]
fn an_address_size_override_clears_the_upper_half_of_the_rep_count() {
    for (name, bytes) in [
        ("rep stosb", vec![0x67, 0xf3, 0xaa, 0xc3]),
        ("repe cmpsb", vec![0x67, 0xf3, 0xa6, 0xc3]),
        ("repne scasb", vec![0x67, 0xf2, 0xae, 0xc3]),
    ] {
        assert!(
            !keeps_the_callers_high_half(bytes),
            "0x67 {name} kept the caller's high 32 bits",
        );
    }
}
