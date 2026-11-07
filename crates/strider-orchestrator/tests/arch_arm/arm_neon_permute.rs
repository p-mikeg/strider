//! `vtrn`, `vzip` and `vuzp` write both operands: each reads Dd and Dm (or Qd
//! and Qm) and permutes their elements across the pair.
//!
//! The words are gas's (`-mcpu=cortex-a9`). A prelude loads d0-d3 from r0-r7
//! and an epilogue returns one D register in r0 and r1, so each run feeds the
//! interpreter core registers.

use crate::common::{Arch, driver_for_reader};
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, FunctionBound, MachineInsnAddr};
use strider_ir::{Function, UBig};
use strider_ir_test_utils::Rng;
use strider_ir_test_utils::interp::{self, EntryVar, Env, Outcome, Slot};

const BASE: u64 = 0x1000;

/// `vmov d0, r0, r1` .. `vmov d3, r6, r7`.
const PRELUDE: [u32; 4] = [0xec41_0b10, 0xec43_2b11, 0xec45_4b12, 0xec47_6b13];
const BX_LR: u32 = 0xe12f_ff1e;
const THUMB_BX_LR: u16 = 0x4770;

/// `vmov r0, r1, dN`.
fn vmov_r0_r1(n: u32) -> u32 {
    0xec51_0b10 | n
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Trn,
    Zip,
    Uzp,
}

/// The A32 word; the Thumb word is the same with `0xff` for `0xf3`.
const CASES: [(&str, u32, Op, bool, u32); 15] = [
    ("vtrn.8 d0, d1", 0xf3b2_0081, Op::Trn, false, 1),
    ("vtrn.16 d0, d1", 0xf3b6_0081, Op::Trn, false, 2),
    ("vtrn.32 d0, d1", 0xf3ba_0081, Op::Trn, false, 4),
    ("vzip.8 d0, d1", 0xf3b2_0181, Op::Zip, false, 1),
    ("vzip.16 d0, d1", 0xf3b6_0181, Op::Zip, false, 2),
    ("vuzp.8 d0, d1", 0xf3b2_0101, Op::Uzp, false, 1),
    ("vuzp.16 d0, d1", 0xf3b6_0101, Op::Uzp, false, 2),
    ("vtrn.8 q0, q1", 0xf3b2_00c2, Op::Trn, true, 1),
    ("vtrn.16 q0, q1", 0xf3b6_00c2, Op::Trn, true, 2),
    ("vtrn.32 q0, q1", 0xf3ba_00c2, Op::Trn, true, 4),
    ("vzip.8 q0, q1", 0xf3b2_01c2, Op::Zip, true, 1),
    ("vzip.16 q0, q1", 0xf3b6_01c2, Op::Zip, true, 2),
    ("vzip.32 q0, q1", 0xf3ba_01c2, Op::Zip, true, 4),
    ("vuzp.8 q0, q1", 0xf3b2_0142, Op::Uzp, true, 1),
    ("vuzp.16 q0, q1", 0xf3b6_0142, Op::Uzp, true, 2),
];

/// Element `i` of `esize` bytes.
fn elem(x: u128, esize: u32, i: u32) -> u128 {
    x >> (8 * esize * i) & (u128::MAX >> (128 - 8 * esize))
}

/// The Arm ARM's VTRN / VZIP / VUZP on Vd = `a`, Vm = `b` of `bytes` bytes.
fn permute(op: Op, esize: u32, bytes: u32, a: u128, b: u128) -> (u128, u128) {
    let n = bytes / esize;
    // Element `i` of the concatenation b:a, a lowest.
    let pair = |i: u32| {
        if i < n {
            elem(a, esize, i)
        } else {
            elem(b, esize, i - n)
        }
    };
    // Element `i` of a and b interleaved, a's first.
    let zip = |i: u32| elem(if i.is_multiple_of(2) { a } else { b }, esize, i / 2);
    let (mut d, mut m) = (0, 0);
    for i in 0..n {
        let (x, y) = match op {
            Op::Trn if i.is_multiple_of(2) => (elem(a, esize, i), elem(a, esize, i + 1)),
            Op::Trn => (elem(b, esize, i - 1), elem(b, esize, i)),
            Op::Zip => (zip(i), zip(n + i)),
            Op::Uzp => (pair(2 * i), pair(2 * i + 1)),
        };
        d |= x << (8 * esize * i);
        m |= y << (8 * esize * i);
    }
    (d, m)
}

/// d0-d3 after `case` entered with `d`.
fn reference(op: Op, quad: bool, esize: u32, d: &[u64; 4]) -> [u64; 4] {
    let mut out = *d;
    let mut apply = |vd: usize, vm: usize, regs: usize| {
        let read = |r: usize| (0..regs).fold(0u128, |x, k| x | u128::from(d[r + k]) << (64 * k));
        let (nd, nm) = permute(op, esize, 8 * regs as u32, read(vd), read(vm));
        for k in 0..regs {
            out[vd + k] = (nd >> (64 * k)) as u64;
            out[vm + k] = (nm >> (64 * k)) as u64;
        }
    };
    match (op, quad) {
        (_, false) => apply(0, 1, 1),
        // A quadword vtrn transposes each D register pair on its own.
        (Op::Trn, true) => {
            apply(0, 2, 1);
            apply(1, 3, 1);
        }
        (_, true) => apply(0, 2, 2),
    }
    out
}

/// The raw and the optimised lift of `word` returning dN, with r0-r7's varnodes.
fn lift(arch: Arch, word: u32, n: u32) -> ([Function; 2], Vec<rsleigh::Vn>) {
    let (image, entry) = if arch == Arch::ArmThumb {
        let mut halves: Vec<u16> = Vec::new();
        for w in PRELUDE
            .iter()
            .chain([&(word | 0x0c00_0000), &vmov_r0_r1(n)])
        {
            halves.extend([(w >> 16) as u16, *w as u16]);
        }
        halves.push(THUMB_BX_LR);
        (
            halves.iter().flat_map(|h| h.to_le_bytes()).collect(),
            BASE | 1,
        )
    } else {
        let mut words = PRELUDE.to_vec();
        words.extend([word, vmov_r0_r1(n), BX_LR]);
        let image = crate::common::words_image(&words, arch.sleigh().endianness());
        (image, BASE)
    };
    let size = image.len() as u64;
    let (mut lifter, cc) = driver_for_reader(arch, BufMemReader::new(image, BASE));
    let gprs = (0..8)
        .map(|n| {
            lifter
                .sleigh_regs()
                .name_to_vn(&format!("r{n}"))
                .expect("gpr")
        })
        .collect();
    let opts = CfgOptions {
        bound: FunctionBound::size(size),
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(MachineInsnAddr::from(entry), &opts)
        .unwrap_or_else(|e| panic!("{arch:?} {word:#x}: cfg: {e:#}"));
    assert!(cfg.incomplete().is_empty(), "{:?}", cfg.incomplete());
    let raw = lifter.build_ir(&cfg, cc).expect("build_ir").function;
    let mut opt = raw.clone();
    strider_orchestrator::opt::default_pipeline()
        .run(&mut opt, &strider_orchestrator::opt::OptCtx::new(None))
        .expect("pipeline");
    ([raw, opt], gprs)
}

/// The D register `f` returns in r0 and r1, entered with `d` in d0-d3.
fn run(f: &Function, gprs: &[rsleigh::Vn], d: &[u64; 4]) -> Option<u64> {
    let entry = |e: EntryVar| {
        let n = gprs.iter().position(|g| *g == e.vn);
        n.map_or(UBig::from(0x5a5a_5a5au32), |n| {
            UBig::from(d[n / 2] >> (32 * (n % 2)) & 0xffff_ffff)
        })
    };
    let env = Env {
        entry: &entry,
        rom: None,
        seed: 0,
        step_budget: 64,
        private: None,
        keeps: None,
    };
    let slot = |values: &[Slot], vn: rsleigh::Vn| {
        let i = f.ret_val_regs().iter().position(|r| *r == vn)?;
        match &values[i] {
            Slot::Known(v) => u64::try_from(v).ok(),
            _ => None,
        }
    };
    match interp::run(f, &env) {
        Ok(Outcome::Return { values, .. }) => {
            Some(slot(&values, gprs[0])? | slot(&values, gprs[1])? << 32)
        }
        // An opaque CallOther on the path: no value.
        Err(interp::Unsupported(_)) => None,
        other => panic!("run: {other:?}"),
    }
}

#[test]
fn vtrn_vzip_and_vuzp_write_both_registers() {
    let mut rng = Rng(0x7e2a_9b1f_0000_0001);
    let mut wrong = Vec::new();
    for arch in [Arch::Arm, Arch::ArmBe, Arch::ArmThumb] {
        for (name, word, op, quad, esize) in CASES {
            for n in 0..4 {
                let (fs, gprs) = lift(arch, word, n);
                for _ in 0..4 {
                    let d = [(); 4].map(|()| rng.next_u64());
                    let want = reference(op, quad, esize, &d)[n as usize];
                    for (f, stage) in fs.iter().zip(["raw", "opt"]) {
                        let got = run(f, &gprs, &d);
                        if got != Some(want) {
                            wrong.push(format!(
                                "{arch:?} {name} d{n} {stage} {d:x?}: {got:x?} want {want:#x}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong: {:#?}",
        wrong.len(),
        &wrong[..wrong.len().min(12)]
    );
}
