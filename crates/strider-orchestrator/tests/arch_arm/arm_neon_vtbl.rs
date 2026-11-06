//! `vtbl.8 Dd, {list}, Dm` writes each byte of Dd from the list's bytes (the
//! first register lowest) at the index in Dm's byte; an index past the list
//! gives 0, and `vtbx.8` keeps Dd's byte instead. Core registers stay put.
//!
//! The Thumb words are gcc's (`-O2 -mfpu=neon -mfloat-abi=hard`) for
//! `vtbl4_u8` and `vtbx1_u8`. A prelude loads d0-d4 from r0-r9 and an epilogue
//! returns d0 in r0 and r1, so each run feeds the interpreter core registers.

use crate::common::{Arch, driver_for_reader};
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, FunctionBound, MachineInsnAddr};
use strider_ir::{Function, UBig};
use strider_ir_test_utils::Rng;
use strider_ir_test_utils::interp::{self, EntryVar, Env, Outcome, Slot};

const BASE: u64 = 0x1000;

/// `vmov d0, r0, r1` .. `vmov d4, r8, r9`.
const PRELUDE: [u32; 5] = [
    0xec41_0b10,
    0xec43_2b11,
    0xec45_4b12,
    0xec47_6b13,
    0xec49_8b14,
];
/// `vmov r0, r1, d0`.
const VMOV_R0_R1_D0: u32 = 0xec51_0b10;
const BX_LR: u32 = 0xe12f_ff1e;
const THUMB_BX_LR: u16 = 0x4770;

/// One instruction: its A32 word and its Thumb word (halfwords high first).
struct Case {
    name: &'static str,
    a32: u32,
    thumb: u32,
    /// The table's first register and length.
    table: (usize, usize),
    vtbx: bool,
}

const CASES: [Case; 4] = [
    Case {
        name: "vtbl.8 d0, {d0-d3}, d4",
        a32: 0xf3b0_0b04,
        thumb: 0xffb0_0b04,
        table: (0, 4),
        vtbx: false,
    },
    Case {
        name: "vtbx.8 d0, {d1}, d2",
        a32: 0xf3b1_0842,
        thumb: 0xffb1_0842,
        table: (1, 1),
        vtbx: true,
    },
    Case {
        name: "vtbl.8 d0, {d1, d2}, d4",
        a32: 0xf3b1_0904,
        thumb: 0xffb1_0904,
        table: (1, 2),
        vtbx: false,
    },
    Case {
        name: "vtbx.8 d0, {d1-d3}, d4",
        a32: 0xf3b1_0a44,
        thumb: 0xffb1_0a44,
        table: (1, 3),
        vtbx: true,
    },
];

/// The Arm ARM's VTBL/VTBX: `d0` after `case` with `d` in d0-d4.
fn reference(case: &Case, d: &[u64; 5]) -> u64 {
    let (first, len) = case.table;
    let index = match case.a32 & 0xf {
        2 => d[2],
        _ => d[4],
    };
    (0..8).fold(0, |out, lane| {
        let i = (index >> (8 * lane)) as usize & 0xff;
        let byte = if i < 8 * len {
            d[first + i / 8] >> (8 * (i % 8)) & 0xff
        } else if case.vtbx {
            d[0] >> (8 * lane) & 0xff
        } else {
            0
        };
        out | byte << (8 * lane)
    })
}

/// The raw and the optimised lift of `case` on `arch`, with r0-r9's varnodes.
fn lift(arch: Arch, case: &Case) -> ([Function; 2], Vec<rsleigh::Vn>) {
    let (image, entry) = if arch == Arch::ArmThumb {
        let mut halves: Vec<u16> = Vec::new();
        for w in PRELUDE.iter().chain([&case.thumb, &VMOV_R0_R1_D0]) {
            halves.extend([(w >> 16) as u16, *w as u16]);
        }
        halves.push(THUMB_BX_LR);
        let image = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
        (image, BASE | 1)
    } else {
        let mut words = PRELUDE.to_vec();
        words.extend([case.a32, VMOV_R0_R1_D0, BX_LR]);
        let image = crate::common::words_image(&words, arch.sleigh().endianness());
        (image, BASE)
    };
    let size = image.len() as u64;
    let (mut lifter, cc) = driver_for_reader(arch, BufMemReader::new(image, BASE));
    let gprs = (0..10)
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
        .unwrap_or_else(|e| panic!("{arch:?} {}: cfg: {e:#}", case.name));
    assert!(cfg.incomplete().is_empty(), "{:?}", cfg.incomplete());
    let raw = lifter.build_ir(&cfg, cc).expect("build_ir").function;
    let mut opt = raw.clone();
    strider_orchestrator::opt::default_pipeline()
        .run(&mut opt, &strider_orchestrator::opt::OptCtx::new(None))
        .expect("pipeline");
    ([raw, opt], gprs)
}

/// `d0` as `f` returns it in r0 and r1, entered with `d` in d0-d4.
fn run(f: &Function, gprs: &[rsleigh::Vn], d: &[u64; 5]) -> u64 {
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
        let i = f
            .ret_val_regs()
            .iter()
            .position(|r| *r == vn)
            .expect("ret reg");
        match &values[i] {
            Slot::Known(v) => u64::try_from(v).unwrap(),
            other => panic!("return slot {other:?}"),
        }
    };
    match interp::run(f, &env) {
        Ok(Outcome::Return { values, .. }) => slot(&values, gprs[0]) | slot(&values, gprs[1]) << 32,
        other => panic!("run: {other:?}"),
    }
}

#[test]
fn vtbl_and_vtbx_look_up_bytes_of_their_d_register_table() {
    let mut rng = Rng(0x7ab1_e5ee_d000_0001);
    let mut wrong = Vec::new();
    for arch in [Arch::Arm, Arch::ArmBe, Arch::ArmThumb] {
        for case in &CASES {
            let (fs, gprs) = lift(arch, case);
            for _ in 0..16 {
                let mut d = [0u64; 5];
                for v in &mut d {
                    *v = rng.next_u64();
                }
                // Index bytes in and past every table length.
                for lane in 0..8 {
                    let i = if rng.chance(10) {
                        0xff
                    } else {
                        rng.below(40) as u64
                    };
                    for k in [2, 4] {
                        d[k] = d[k] & !(0xff << (8 * lane)) | i << (8 * lane);
                    }
                }
                let want = reference(case, &d);
                for (f, stage) in fs.iter().zip(["raw", "opt"]) {
                    let got = run(f, &gprs, &d);
                    if got != want {
                        wrong.push(format!(
                            "{arch:?} {} {stage} {d:x?}: {got:#x} want {want:#x}",
                            case.name
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
