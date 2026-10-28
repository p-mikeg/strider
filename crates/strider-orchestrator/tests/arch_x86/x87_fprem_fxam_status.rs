//! x87 `fxam` classifies ST0 from its encoding (SDM FXAM: C3, C2 and C0 the
//! class, C1 the sign), `fcom` sets C0-C3 and leaves the status word's other
//! bits alone, and `fprem` and `fprem1` are opaque user ops whose status
//! word the loop around them tests.

use crate::common;

use strider_ir::{Function, IRViewer, IRWalker};

const BASE: u64 = 0x10000;
const STRIDE: usize = 64;
const LIT: usize = 48;

/// Bits FXAM and FCOM leave as they were: all but C0-C3.
const KEPT: u64 = 0xb8ff;

fn f80(sign: u128, exp: u128, mant: u128) -> u128 {
    sign << 79 | exp << 64 | mant
}

/// Each encoding with its C3 C2 C0 class bits and its sign (C1).
fn classes() -> Vec<(&'static str, u128, u64)> {
    const J: u128 = 1 << 63;
    let c = |c3: u64, c2: u64, c0: u64| c3 << 14 | c2 << 10 | c0 << 8;
    let mut v = Vec::new();
    for sign in [0, 1] {
        let c1 = (sign as u64) << 9;
        v.extend([
            ("zero", f80(sign, 0, 0), c(1, 0, 0) | c1),
            ("normal", f80(sign, 0x3fff, J), c(0, 1, 0) | c1),
            ("normal 2.5", f80(sign, 0x4000, J | J >> 2), c(0, 1, 0) | c1),
            ("infinity", f80(sign, 0x7fff, J), c(0, 1, 1) | c1),
            ("quiet nan", f80(sign, 0x7fff, J | J >> 1), c(0, 0, 1) | c1),
            ("signalling nan", f80(sign, 0x7fff, J | 1), c(0, 0, 1) | c1),
            ("denormal", f80(sign, 0, 1), c(1, 1, 0) | c1),
            ("pseudo-denormal", f80(sign, 0, J | 1), c(1, 1, 0) | c1),
            ("unnormal", f80(sign, 0x3fff, 1), c1),
            ("pseudo-infinity", f80(sign, 0x7fff, 0), c1),
            ("pseudo-nan", f80(sign, 0x7fff, 1), c1),
        ]);
    }
    v
}

/// One function per literal: `fld tbyte [rip+lit]; body; fnstsw ax;
/// movzx eax, ax; ret`.
fn image(body: &[u8], lits: &[u128]) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = vec![0; lits.len() * STRIDE];
    let mut entries = Vec::new();
    for (i, &lit) in lits.iter().enumerate() {
        let at = i * STRIDE;
        let disp = u32::try_from(LIT - 6).unwrap();
        let mut code = vec![0xdb, 0x2d];
        code.extend_from_slice(&disp.to_le_bytes());
        code.extend_from_slice(body);
        code.extend_from_slice(&[0xdf, 0xe0, 0x0f, 0xb7, 0xc0, 0xc3]);
        assert!(code.len() <= LIT);
        bytes[at..at + code.len()].copy_from_slice(&code);
        bytes[at + LIT..at + LIT + 10].copy_from_slice(&lit.to_le_bytes()[..10]);
        entries.push(BASE + at as u64);
    }
    (bytes, entries)
}

fn status_words(body: &[u8], lits: &[u128], init: u128) -> Vec<u64> {
    let (bytes, entries) = image(body, lits);
    common::cone::returned_at_under(
        common::Arch::X64,
        bytes,
        BASE,
        &entries,
        init,
        &Default::default(),
    )
}

#[test]
fn fxam_classifies_from_the_encoding_and_keeps_the_other_bits() {
    let classes = classes();
    let lits: Vec<u128> = classes.iter().map(|c| c.1).collect();
    for init in [0, u128::MAX] {
        // fxam
        let got = status_words(&[0xd9, 0xe5], &lits, init);
        for ((name, bits, class), got) in classes.iter().zip(got) {
            let want = (init as u64 & KEPT) | class;
            assert_eq!(got, want, "fxam of {name} {bits:#x}, init {init:#x}");
        }
    }
}

/// `fld1; fcom st(1)` against 2.0, 1.0 and 0.5 in ST1: C0 for less, C3 for
/// equal, neither for greater, C1 and C2 clear.
#[test]
fn fcom_keeps_the_status_words_other_bits() {
    const J: u128 = 1 << 63;
    let cases = [
        (f80(0, 0x4000, J), 1u64 << 8),
        (f80(0, 0x3fff, J), 1 << 14),
        (f80(0, 0x3ffe, J), 0),
    ];
    let lits: Vec<u128> = cases.iter().map(|c| c.0).collect();
    for init in [0, u128::MAX] {
        // fld1; fcom st(1)
        let got = status_words(&[0xd9, 0xe8, 0xd8, 0xd1], &lits, init);
        for (&(st1, c), got) in cases.iter().zip(got) {
            assert_eq!(
                got,
                (init as u64 & KEPT) | c,
                "fcom against {st1:#x}, init {init:#x}"
            );
        }
    }
}

fn analyze(bytes: Vec<u8>) -> strider_orchestrator::AnalyzeResult {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
}

/// Whether `name`'s `CallOther` is in the cone of `v`.
fn reaches(f: &Function, v: strider_ir::node::ValueId, name: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut work = vec![v];
    while let Some(v) = work.pop() {
        let node = f.producer(v);
        if !seen.insert(node) {
            continue;
        }
        if f.call_other_name(node) == Some(name) {
            return true;
        }
        work.extend(f.node_inputs(node));
    }
    false
}

/// glibc's `__fmodl_finite`: `fldt 0x18(%rsp); fldt 0x8(%rsp); 1: fprem;
/// fnstsw %ax; testb $4, %ah; jne 1b; fstp %st(1); ret`. The loop exits on
/// the C2 the remainder op reports, and the result is the op's.
#[test]
fn the_fprem_loop_tests_the_status_word_fprem_leaves() {
    for (op, rem, status) in [
        (0xf8, "x87Prem", "x87PremStatus"),
        (0xf5, "x87Prem1", "x87Prem1Status"),
    ] {
        let bytes = vec![
            0xdb, 0x6c, 0x24, 0x18, 0xdb, 0x6c, 0x24, 0x08, 0xd9, op, 0xdf, 0xe0, 0xf6, 0xc4, 0x04,
            0x75, 0xf7, 0xdd, 0xd9, 0xc3,
        ];
        let r = analyze(bytes);
        assert!(r.cfg.incomplete().is_empty(), "{:?}", r.cfg.incomplete());
        let f = &r.function;
        let ifs: Vec<_> = f
            .walk()
            .filter(|&n| matches!(f.node_kind(n), strider_ir::node::NodeKind::If))
            .collect();
        assert_eq!(ifs.len(), 1, "{}", f.to_text(false));
        let cond = f.node_inputs(ifs[0])[1];
        assert!(reaches(f, cond, status), "{}", f.to_text(false));
        assert!(f.walk().any(|n| f.call_other_name(n) == Some(rem)), "{rem}");
    }
}

/// The remainder ops close the list on both x86 presets, only
/// `x87RoundingControl` after them, so no other id moved.
#[test]
fn the_remainder_ops_are_the_last_user_ops() {
    for arch in [common::Arch::X86, common::Arch::X64] {
        let (strider, _) = common::strider_over_bytes(arch, vec![0xc3], BASE, None);
        let names = strider.user_op_names();
        let tail: Vec<&str> = names[names.len() - 6..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "mxcsrRoundingControl",
                "x87PremStatus",
                "x87Prem",
                "x87Prem1Status",
                "x87Prem1",
                "x87RoundingControl"
            ],
            "{arch:?}"
        );
    }
}
