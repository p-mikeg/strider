//! PowerPC round-to-integral (`friz`, `frin`, `frip`, `frim`, Power ISA 3.0B
//! 4.6.6.2) and the VSX scalar forms' lanes. `friz` rounds toward zero to an
//! integral double, keeping the sign of a zero. A VSX scalar reads and writes
//! doubleword 0 of a VSR, which the FPR of the same number overlays;
//! `xscvdpspn` writes the single to word 0 (and word 1, which `mfvsrwz`
//! reads back).
//!
//! Each image loads the doubleword at 0x10040 into f1 (`lis r9, 1; lfd f1,
//! 0x40(r9)`), runs the body, and returns r3.

mod common;

use common::Arch;
use strider_orchestrator::opt::AssumptionOptions;

const BASE: u64 = 0x10000;
const DATA: usize = 0x40;
const ARCHES: [Arch; 2] = [Arch::Ppc64be, Arch::Ppc64le];
const LOAD_F1: [u32; 2] = [0x3d20_0001, 0xc829_0040];
/// `mffprd r3, f1`.
const MFFPRD_R3_F1: u32 = 0x7c23_0066;
const BLR: u32 = 0x4e80_0020;

fn image(arch: Arch, words: &[u32], data: u64) -> Vec<u8> {
    let endian = arch.sleigh().endianness();
    let mut bytes = common::words_image(words, endian);
    assert!(bytes.len() <= DATA);
    bytes.resize(DATA, 0);
    bytes.extend_from_slice(&match endian {
        strider_target::Endianness::Big => data.to_be_bytes(),
        strider_target::Endianness::Little => data.to_le_bytes(),
    });
    bytes
}

fn returned(arch: Arch, words: &[u32], data: u64) -> u64 {
    common::cone::returned_from_under(
        arch,
        image(arch, words, data),
        BASE,
        0,
        &AssumptionOptions::default(),
    )
}

/// A round-to-integral form: its name, encoding and the host's rounding.
type RoundForm = (&'static str, u32, fn(f64) -> f64);

const VALUES: [f64; 16] = [
    2.5,
    -2.5,
    0.5,
    -0.5,
    3.7,
    -3.7,
    0.0,
    -0.0,
    4_503_599_627_370_495.5,
    -4_503_599_627_370_495.5,
    4_503_599_627_370_497.0,
    9_007_199_254_740_993.0,
    1e300,
    -1e300,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

#[test]
fn round_to_integral_forms_stay_doubles() {
    let forms: [RoundForm; 4] = [
        ("friz", 0xfc20_0b50, f64::trunc),
        ("frin", 0xfc20_0b10, f64::round),
        ("frip", 0xfc20_0b90, f64::ceil),
        ("frim", 0xfc20_0bd0, f64::floor),
    ];
    for arch in ARCHES {
        for (name, word, want) in forms {
            for x in VALUES {
                let words = [LOAD_F1[0], LOAD_F1[1], word, MFFPRD_R3_F1, BLR];
                let got = f64::from_bits(returned(arch, &words, x.to_bits()));
                assert_eq!(
                    got.to_bits(),
                    want(x).to_bits(),
                    "{arch:?} {name} {x}: got {got}"
                );
            }
            let words = [LOAD_F1[0], LOAD_F1[1], word, MFFPRD_R3_F1, BLR];
            let got = f64::from_bits(returned(arch, &words, f64::NAN.to_bits()));
            assert!(got.is_nan(), "{arch:?} {name} NaN: got {got}");
        }
    }
}

/// gcc's float-to-u32 pun (`GET_FLOAT_WORD`): `xscvdpspn vs0, vs1; mfvsrwz
/// r3, vs0` gives the IEEE single's bits.
#[test]
fn xscvdpspn_then_mfvsrwz_gives_the_single_bits() {
    for arch in ARCHES {
        for x in [1.5f32, -2.25, 0.1, f32::MAX, f32::MIN_POSITIVE, -0.0] {
            let words = [LOAD_F1[0], LOAD_F1[1], 0xf000_0c2c, 0x7c03_00e6, BLR];
            let got = returned(arch, &words, f64::from(x).to_bits());
            assert_eq!(got, u64::from(x.to_bits()), "{arch:?} {x}");
        }
    }
}

/// `lxsdx vs1, 0, r9` loads doubleword 0, which f1 reads.
#[test]
fn lxsdx_loads_doubleword_zero() {
    for arch in ARCHES {
        let x = 1.25f64;
        let words = [0x3d20_0001, 0x3929_0040, 0x7c20_4c98, MFFPRD_R3_F1, BLR];
        assert_eq!(returned(arch, &words, x.to_bits()), x.to_bits(), "{arch:?}");
    }
}

/// `xsadddp vs1, vs1, vs1` adds doubleword 0 into doubleword 0.
#[test]
fn xsadddp_works_in_doubleword_zero() {
    for arch in ARCHES {
        let words = [LOAD_F1[0], LOAD_F1[1], 0xf021_0900, MFFPRD_R3_F1, BLR];
        assert_eq!(
            returned(arch, &words, 1.25f64.to_bits()),
            2.5f64.to_bits(),
            "{arch:?}"
        );
    }
}
