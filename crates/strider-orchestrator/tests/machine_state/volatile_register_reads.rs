//! A hardware counter changes between two reads with nothing writing it, so
//! each read is its own opaque value and `t1 - t0` stays unknown. A register
//! no spec marks volatile still reads as one SSA value.

use crate::common;

use crate::common::{Arch, returned, words_image};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::Endianness;

const BASE: u64 = 0x1000;

const AARCH64: [Arch; 2] = [Arch::Aarch64, Arch::Aarch64Be];
const PPC: [Arch; 4] = [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le];
const MIPS: [Arch; 4] = [
    Arch::Mips32be,
    Arch::Mips32le,
    Arch::Mips64be,
    Arch::Mips64le,
];

fn analyze(arch: Arch, words: &[u32]) -> strider_ir::Function {
    // AArch64 instructions are little-endian on both presets.
    let endian = match arch {
        Arch::Aarch64 | Arch::Aarch64Be => Endianness::Little,
        _ => arch.sleigh().endianness(),
    };
    let bytes = words_image(words, endian);
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    strider
        .analyze(BASE, &cc, &Default::default(), None)
        .unwrap_or_else(|e| panic!("{arch:?}: {e}"))
        .function
}

fn volatile_reads(f: &strider_ir::Function) -> usize {
    f.walk()
        .filter(|&n| {
            matches!(f.node_kind(n), NodeKind::CallOther { .. })
                && f.call_other_name(n) == Some("read_volatile")
        })
        .count()
}

/// The first returned register is `t1 - t0`: unknown, from two reads.
fn assert_elapsed_is_opaque(what: &str, arch: Arch, words: &[u32]) {
    let f = analyze(arch, words);
    let elapsed = returned(&f)[0];
    assert_eq!(
        f.int_const_u128(elapsed),
        None,
        "{what} on {arch:?}: two counter reads folded to one value"
    );
    assert_eq!(volatile_reads(&f), 2, "{what} on {arch:?}");
}

/// The first returned register is `r - r` of a plain register: zero.
fn assert_difference_folds(what: &str, arch: Arch, words: &[u32]) {
    let f = analyze(arch, words);
    assert_eq!(
        f.int_const_u128(returned(&f)[0]),
        Some(0),
        "{what} on {arch:?}"
    );
    assert_eq!(volatile_reads(&f), 0, "{what} on {arch:?}");
}

/// `mfspr r10, X; mfspr r9, X; subf r3, r10, r9; blr` for each counter X.
const PPC_COUNTERS: [(&str, [u32; 2]); 6] = [
    ("mftb", [0x7d4c_42a6, 0x7d2c_42a6]),
    ("mfdec", [0x7d56_02a6, 0x7d36_02a6]),
    ("pmc1 (787)", [0x7d53_c2a6, 0x7d33_c2a6]),
    ("purr (309)", [0x7d55_4aa6, 0x7d35_4aa6]),
    ("vtb (849)", [0x7d51_d2a6, 0x7d31_d2a6]),
    ("pmc7 (793)", [0x7d59_c2a6, 0x7d39_c2a6]),
];

/// `mfc0 t0, X; mfc0 t1, X; subu v0, t1, t0; jr ra; nop` for each X.
const MIPS_COUNTERS: [(&str, [u32; 2]); 4] = [
    ("count $9", [0x4008_4800, 0x4009_4800]),
    ("random $1", [0x4008_0800, 0x4009_0800]),
    ("cause $13", [0x4008_6800, 0x4009_6800]),
    ("perf counter $25, 1", [0x4008_c801, 0x4009_c801]),
];

/// As [`MIPS_COUNTERS`], for registers no spec marks volatile.
const MIPS_PLAIN: [(&str, [u32; 2]); 2] = [
    ("status $12", [0x4008_6000, 0x4009_6000]),
    ("perf control $25, 0", [0x4008_c800, 0x4009_c800]),
];

const PPC_TAIL: [u32; 2] = [0x7c6a_4850, 0x4e80_0020];
const MIPS_TAIL: [u32; 3] = [0x0128_1023, 0x03e0_0008, 0];

fn with_tail(reads: [u32; 2], tail: &[u32]) -> Vec<u32> {
    reads.iter().chain(tail).copied().collect()
}

#[test]
fn powerpc_counter_reads_differ() {
    for (name, reads) in PPC_COUNTERS {
        let words = with_tail(reads, &PPC_TAIL);
        for arch in PPC {
            assert_elapsed_is_opaque(name, arch, &words);
        }
    }
}

#[test]
fn mips_counter_reads_differ() {
    for (name, reads) in MIPS_COUNTERS {
        let words = with_tail(reads, &MIPS_TAIL);
        for arch in MIPS {
            assert_elapsed_is_opaque(name, arch, &words);
        }
    }
}

#[test]
fn mips_plain_cp0_reads_are_one_value() {
    for (name, reads) in MIPS_PLAIN {
        let words = with_tail(reads, &MIPS_TAIL);
        for arch in MIPS {
            assert_difference_folds(name, arch, &words);
        }
    }
}

/// `sp_el0` is the kernel's `current`, which every writer a call can reach
/// restores before returning, so two reads in one function are one value.
#[test]
fn aarch64_sp_el0_reads_are_one_value() {
    // mrs x0, sp_el0; mrs x1, sp_el0; sub x0, x1, x0; ret
    let words = [0xd538_4100, 0xd538_4101, 0xcb00_0020, 0xd65f_03c0];
    for arch in AARCH64 {
        assert_difference_folds("sp_el0", arch, &words);
    }
}

/// A callee returns to the same task, so `current` survives a call.
#[test]
fn aarch64_sp_el0_survives_a_call() {
    // mrs x19, sp_el0; bl +0x400; mrs x1, sp_el0; sub x0, x19, x1; ret
    let words = [
        0xd538_4113,
        0x9400_0100,
        0xd538_4101,
        0xcb01_0260,
        0xd65f_03c0,
    ];
    for arch in AARCH64 {
        assert_difference_folds("sp_el0 across a call", arch, &words);
    }
}

#[test]
fn aarch64_virtual_counter_reads_differ() {
    // mrs x0, cntvct_el0; mrs x1, cntvct_el0; sub x0, x1, x0; ret
    let words = [0xd53b_e040, 0xd53b_e041, 0xcb00_0020, 0xd65f_03c0];
    for arch in AARCH64 {
        assert_elapsed_is_opaque("cntvct_el0", arch, &words);
    }
}

#[test]
fn powerpc_sprg0_reads_are_one_value() {
    // mfspr r10, 272; mfspr r9, 272; subf r3, r10, r9; blr
    let words = [0x7d50_42a6, 0x7d30_42a6, 0x7c6a_4850, 0x4e80_0020];
    for arch in PPC {
        assert_difference_folds("sprg0", arch, &words);
    }
}

/// e500 `mfpmr` reads a performance-monitor register through a user op,
/// which only the ppc64 presets decode; `mtpmr` writes one.
#[test]
fn powerpc_mfpmr_reads_differ() {
    // mfpmr r3, 16; mfpmr r4, 16; mtpmr 16, r5; subf r3, r4, r3; blr
    let words = [
        0x7c70_029c,
        0x7c90_029c,
        0x7cb0_039c,
        0x7c64_1850,
        0x4e80_0020,
    ];
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = analyze(arch, &words);
        assert_eq!(
            f.int_const_u128(returned(&f)[0]),
            None,
            "{arch:?}: two mfpmr reads folded to one value"
        );
        let reads = f
            .walk()
            .filter(|&n| f.call_other_name(n) == Some("moveFromPerformanceMonitorRegister"))
            .count();
        assert_eq!(reads, 2, "{arch:?}");
    }
}

/// ARM reads the generic timer through `mrrc`, which the sla lifts as a
/// `CallOther` per read, so two reads are two nodes without any volatile list.
#[test]
fn arm_generic_timer_reads_differ() {
    // mrrc p15, 1, r0, r1, c14; mrrc p15, 1, r2, r3, c14; subs r0, r2, r0; bx lr
    let f = analyze(
        Arch::Arm,
        &[0xec51_0f1e, 0xec53_2f1e, 0xe052_0000, 0xe12f_ff1e],
    );
    assert_eq!(f.int_const_u128(returned(&f)[0]), None);
    let low_word_reads = f
        .walk()
        .filter(|&n| f.call_other_name(n) == Some("coprocessor_movefromRt"))
        .count();
    assert_eq!(low_word_reads, 2);
}

/// The region `n`'s control descends from, past straight-line control nodes.
fn enclosing_region(
    f: &strider_ir::Function,
    n: strider_ir::node::NodeId,
) -> strider_ir::node::NodeId {
    let mut n = n;
    while !matches!(f.node_kind(n), NodeKind::Region | NodeKind::Entry) {
        n = f.producer(f.node_inputs(n)[0]);
    }
    n
}

/// A poll loop's exit test reads the counter afresh each iteration: some `If`
/// tests a `read_volatile` taken in a region that the `If` itself loops back
/// to. Phis are not followed, so a value carried around the loop does not
/// count.
fn assert_loop_exit_rereads(arch: Arch, words: &[u32]) {
    let f = analyze(arch, words);
    let found = f.walk().any(|i| {
        if !matches!(f.node_kind(i), NodeKind::If) {
            return false;
        }
        let exits: Vec<_> = f.node_outputs(i).to_vec();
        let mut stack = vec![f.producer(f.node_inputs(i)[1])];
        let mut seen = std::collections::HashSet::new();
        while let Some(n) = stack.pop() {
            if !seen.insert(n) {
                continue;
            }
            match f.node_kind(n) {
                NodeKind::Phi => {}
                NodeKind::CallOther { .. } if f.call_other_name(n) == Some("read_volatile") => {
                    let header = enclosing_region(&f, n);
                    if f.node_inputs(header)
                        .into_iter()
                        .any(|c| exits.contains(&c))
                    {
                        return true;
                    }
                }
                _ => stack.extend(f.node_inputs(n).into_iter().map(|v| f.producer(v))),
            }
        }
        false
    });
    assert!(found, "{arch:?}: no loop exit re-reads the counter");
}

#[test]
fn mips_count_poll_loop_rereads_each_iteration() {
    // 1: mfc0 t2, $9; sltu t3, t2, a0; bnez t3, 1b; nop; jr ra; move v0, t2
    let words = [
        0x400a_4800,
        0x0144_582b,
        0x1560_fffd,
        0,
        0x03e0_0008,
        0x0140_1025,
    ];
    for arch in MIPS {
        assert_loop_exit_rereads(arch, &words);
    }
}

#[test]
fn aarch64_cntvct_poll_loop_rereads_each_iteration() {
    // mrs x1, cntvct_el0; 1: mrs x2, cntvct_el0; sub x3, x2, x1; cmp x3, x0;
    // b.lo 1b; mov x0, x3; ret
    let words = [
        0xd53b_e041,
        0xd53b_e042,
        0xcb01_0043,
        0xeb00_007f,
        0x54ff_ffa3,
        0xaa03_03e0,
        0xd65f_03c0,
    ];
    for arch in AARCH64 {
        assert_loop_exit_rereads(arch, &words);
    }
}
