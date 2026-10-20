//! A hardware counter changes between two reads with nothing writing it, so
//! each read is its own opaque value and `t1 - t0` stays unknown. A register
//! no spec marks volatile still reads as one SSA value.

mod common;

use common::{Arch, returned, words_image};
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
fn assert_elapsed_is_opaque(arch: Arch, words: &[u32]) {
    let f = analyze(arch, words);
    let elapsed = returned(&f)[0];
    assert_eq!(
        f.int_const_u128(elapsed),
        None,
        "{arch:?}: two counter reads folded to one value"
    );
    assert_eq!(volatile_reads(&f), 2, "{arch:?}");
}

/// The first returned register is `r - r` of a plain register: zero.
fn assert_difference_folds(arch: Arch, words: &[u32]) {
    let f = analyze(arch, words);
    assert_eq!(f.int_const_u128(returned(&f)[0]), Some(0), "{arch:?}");
    assert_eq!(volatile_reads(&f), 0, "{arch:?}");
}

#[test]
fn aarch64_virtual_counter_reads_differ() {
    // mrs x0, cntvct_el0; mrs x1, cntvct_el0; sub x0, x1, x0; ret
    let words = [0xd53b_e040, 0xd53b_e041, 0xcb00_0020, 0xd65f_03c0];
    for arch in AARCH64 {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn powerpc_timebase_reads_differ() {
    // mftb r10; mftb r9; subf r3, r10, r9; blr
    let words = [0x7d4c_42a6, 0x7d2c_42a6, 0x7c6a_4850, 0x4e80_0020];
    for arch in PPC {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn powerpc_sprg0_reads_are_one_value() {
    // mfspr r10, 272; mfspr r9, 272; subf r3, r10, r9; blr
    let words = [0x7d50_42a6, 0x7d30_42a6, 0x7c6a_4850, 0x4e80_0020];
    for arch in PPC {
        assert_difference_folds(arch, &words);
    }
}

#[test]
fn mips_cp0_count_reads_differ() {
    // mfc0 t0, $9; mfc0 t1, $9; subu v0, t1, t0; jr ra; nop
    let words = [0x4008_4800, 0x4009_4800, 0x0128_1023, 0x03e0_0008, 0];
    for arch in MIPS {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn mips_cp0_random_reads_differ() {
    // mfc0 t0, $1; mfc0 t1, $1; subu v0, t1, t0; jr ra; nop
    let words = [0x4008_0800, 0x4009_0800, 0x0128_1023, 0x03e0_0008, 0];
    for arch in MIPS {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn mips_cp0_status_reads_are_one_value() {
    // mfc0 t0, $12; mfc0 t1, $12; subu v0, t1, t0; jr ra; nop
    let words = [0x4008_6000, 0x4009_6000, 0x0128_1023, 0x03e0_0008, 0];
    for arch in MIPS {
        assert_difference_folds(arch, &words);
    }
}

#[test]
fn powerpc_decrementer_reads_differ() {
    // mfdec r10; mfdec r9; subf r3, r10, r9; blr
    let words = [0x7d56_02a6, 0x7d36_02a6, 0x7c6a_4850, 0x4e80_0020];
    for arch in PPC {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn powerpc_pmc1_reads_differ() {
    // mfspr r10, 787; mfspr r9, 787; subf r3, r10, r9; blr
    let words = [0x7d53_c2a6, 0x7d33_c2a6, 0x7c6a_4850, 0x4e80_0020];
    for arch in PPC {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn powerpc_purr_reads_differ() {
    // mfspr r10, 309; mfspr r9, 309; subf r3, r10, r9; blr
    let words = [0x7d55_4aa6, 0x7d35_4aa6, 0x7c6a_4850, 0x4e80_0020];
    for arch in PPC {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn mips_performance_counter_reads_differ() {
    // mfc0 t0, $25, 1; mfc0 t1, $25, 1; subu v0, t1, t0; jr ra; nop
    let words = [0x4008_c801, 0x4009_c801, 0x0128_1023, 0x03e0_0008, 0];
    for arch in MIPS {
        assert_elapsed_is_opaque(arch, &words);
    }
}

#[test]
fn mips_performance_control_reads_are_one_value() {
    // mfc0 t0, $25, 0; mfc0 t1, $25, 0; subu v0, t1, t0; jr ra; nop
    let words = [0x4008_c800, 0x4009_c800, 0x0128_1023, 0x03e0_0008, 0];
    for arch in MIPS {
        assert_difference_folds(arch, &words);
    }
}

/// ARM reads the generic timer through `mrrc`, which the sla lifts as a
/// `CallOther`, so two reads are two nodes without any volatile list.
#[test]
fn arm_generic_timer_reads_differ() {
    // mrrc p15, 1, r0, r1, c14; mrrc p15, 1, r2, r3, c14; subs r0, r2, r0; bx lr
    let f = analyze(
        Arch::Arm,
        &[0xec51_0f1e, 0xec53_2f1e, 0xe052_0000, 0xe12f_ff1e],
    );
    assert_eq!(f.int_const_u128(returned(&f)[0]), None);
    assert_eq!(volatile_reads(&f), 0);
}
