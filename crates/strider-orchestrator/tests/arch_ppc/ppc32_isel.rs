//! `isel` decodes on the 32-bit PowerPC presets and selects by its CR bit.
//!
//! The words are clang's, from a ppc32 Linux 6.6 `vmlinux`:
//!
//! ```text
//! is_valid_bugaddr:                 __pick_first_entity, past its load:
//!   54 63 17 be  srwi   r3,r3,30      28 03 00 00  cmplwi r3,0
//!   38 80 00 01  li     r4,1          38 63 ff f8  addi   r3,r3,-8
//!   28 03 00 02  cmplwi r3,2          7c 60 18 9e  iseleq r3,0,r3
//!   38 60 00 00  li     r3,0          4e 80 00 20  blr
//!   7c 64 18 5e  iselgt r3,r4,r3
//!   4e 80 00 20  blr
//! ```
//!
//! The second reads `RA = 0` as the constant zero, not `r0`.

use crate::common::{Arch, driver_for_reader};
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, FunctionBound, MachineInsnAddr};
use strider_ir::{Function, UBig, ValueType, eval};
use strider_ir_test_utils::interp::{self, EntryVar, Env, Outcome, Slot};

const BASE: u64 = 0x1000;

const IS_VALID_BUGADDR: [u32; 6] = [
    0x5463_17be,
    0x3880_0001,
    0x2803_0002,
    0x3860_0000,
    0x7c64_185e,
    0x4e80_0020,
];

const PICK_FIRST_ENTITY: [u32; 4] = [0x2803_0000, 0x3863_fff8, 0x7c60_189e, 0x4e80_0020];

/// The raw and the optimised lift of `words` on `arch`, with `r3`'s varnode.
fn lift(arch: Arch, words: &[u32]) -> ([Function; 2], rsleigh::Vn) {
    let image: Vec<u8> = words
        .iter()
        .flat_map(|w| match arch {
            Arch::Ppc32le => w.to_le_bytes(),
            _ => w.to_be_bytes(),
        })
        .collect();
    let size = image.len() as u64;
    let (mut lifter, cc) = driver_for_reader(arch, BufMemReader::new(image, BASE));
    let r3 = lifter.sleigh_regs().name_to_vn("r3").expect("r3");
    let opts = CfgOptions {
        bound: FunctionBound::size(size),
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(MachineInsnAddr::from(BASE), &opts)
        .unwrap_or_else(|e| panic!("{}: cfg: {e:#}", arch.name()));
    assert!(cfg.incomplete().is_empty(), "{:?}", cfg.incomplete());
    let raw = lifter.build_ir(&cfg, cc).expect("build_ir").function;
    let mut opt = raw.clone();
    strider_orchestrator::opt::default_pipeline()
        .run(&mut opt, &strider_orchestrator::opt::OptCtx::new(None))
        .expect("pipeline");
    ([raw, opt], r3)
}

/// The `r3` `f` returns when entered with `x` in `r3`.
fn returned(f: &Function, r3: rsleigh::Vn, x: u32) -> u32 {
    let entry = |e: EntryVar| {
        if e.vn.addr_space == r3.addr_space && e.vn.addr_off == r3.addr_off {
            UBig::from(x)
        } else {
            UBig::from(0x5a5a_5a5au32)
        }
    };
    let env = Env {
        entry: &entry,
        rom: None,
        seed: 0,
        step_budget: 64,
        private: None,
        keeps: None,
    };
    match interp::run(f, &env) {
        Ok(Outcome::Return { values, .. }) => match values.first() {
            Some(Slot::Known(v)) => u32::try_from(&eval::mask(v, ValueType::int(32))).unwrap(),
            other => panic!("return slot {other:?}"),
        },
        other => panic!("run: {other:?}"),
    }
}

const INPUTS: [u32; 8] = [
    0,
    1,
    8,
    0x7fff_ffff,
    0x8000_0000,
    0xbfff_ffff,
    0xc000_0000,
    0xffff_ffff,
];

#[test]
fn isel_selects_ra_when_its_cr_bit_is_set() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le] {
        let (fs, r3) = lift(arch, &IS_VALID_BUGADDR);
        for f in &fs {
            for x in INPUTS {
                let want = u32::from(x >> 30 > 2);
                assert_eq!(
                    returned(f, r3, x),
                    want,
                    "{} is_valid_bugaddr {x:#x}",
                    arch.name()
                );
            }
        }
    }
}

#[test]
fn isel_reads_ra_zero_as_the_constant_zero() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le] {
        let (fs, r3) = lift(arch, &PICK_FIRST_ENTITY);
        for f in &fs {
            for x in INPUTS {
                let want = if x == 0 { 0 } else { x.wrapping_sub(8) };
                assert_eq!(
                    returned(f, r3, x),
                    want,
                    "{} pick_first_entity {x:#x}",
                    arch.name()
                );
            }
        }
    }
}
