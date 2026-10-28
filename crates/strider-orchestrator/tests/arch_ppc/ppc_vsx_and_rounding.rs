//! PowerPC round-to-integral (`friz`, `frin`, `frip`, `frim`, Power ISA 3.0B
//! 4.6.6.2) and the VSX scalar forms' lanes. `friz` rounds toward zero to an
//! integral double, keeping the sign of a zero. A VSX scalar reads and writes
//! doubleword 0 of a VSR, which the FPR of the same number overlays;
//! `xscvdpspn` writes the single to word 0 (and word 1, which `mfvsrwz`
//! reads back).
//!
//! Each image loads the doubleword at 0x10040 into f1 (`lis r9, 1; lfd f1,
//! 0x40(r9)`), runs the body, and returns r3.

use crate::common;

use crate::common::Arch;
use crate::common::cone::{Val, lift_image, lift_regs};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
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

fn int(v: &Val) -> u128 {
    match v {
        Val::Int(x) => *x,
        other => panic!("{other:?} is no register value"),
    }
}

/// `xscvdpspn vs0, vs1` leaves doubleword 1 of vs0, which the ISA leaves
/// undefined, as it was.
#[test]
fn xscvdpspn_keeps_doubleword_one() {
    for arch in ARCHES {
        let lifted = lift_regs(
            arch,
            &[0xf000_0c2c, BLR],
            &["vs0"],
            &["vs0", "f1"],
            &AssumptionOptions::default(),
        );
        let dw1 = 0x0123_4567_89ab_cdef_u128;
        let x = 1.5f32;
        let got = int(&lifted.run(&[dw1, f64::from(x).to_bits().into()], 0)[0]);
        let w = u128::from(x.to_bits());
        assert_eq!(got, w << 96 | w << 64 | dw1, "{arch:?}: got {got:#x}");
    }
}

/// `lxsd v2, 64(r4)` loads doubleword 0 of vs34 and zeroes doubleword 1.
#[test]
fn lxsd_loads_doubleword_zero_and_zeroes_doubleword_one() {
    for arch in ARCHES {
        let endian = arch.sleigh().endianness();
        let mut bytes = common::words_image(&[0x3c80_0001, 0xe444_0042, BLR], endian);
        bytes.resize(DATA, 0);
        let mem = 0x403f_3e3d_3c3b_3a39_u64;
        bytes.extend_from_slice(&match endian {
            strider_target::Endianness::Big => mem.to_be_bytes(),
            strider_target::Endianness::Little => mem.to_le_bytes(),
        });
        let lifted = lift_image(arch, bytes, &["vs34"], &[], &AssumptionOptions::default());
        let got = int(&lifted.run(&[], u128::MAX)[0]);
        assert_eq!(got, u128::from(mem) << 64, "{arch:?}: got {got:#x}");
    }
}

/// `stxvw4x vs1, 0, r4` stores word i of vs1 at r4 + 4i, each in the
/// target's byte order, as `lxvw4x` loads them.
#[test]
fn stxvw4x_stores_word_zero_first() {
    // addi r4, r1, -64; stxvw4x vs1, 0, r4; lwz r3/r5/r6/r7, 0/4/8/12(r4)
    let words = [
        0x3881_ffc0,
        0x7c20_2718,
        0x8064_0000,
        0x80a4_0004,
        0x80c4_0008,
        0x80e4_000c,
        BLR,
    ];
    let vs1 = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff_u128;
    for arch in ARCHES {
        let lifted = lift_regs(
            arch,
            &words,
            &["r3", "r5", "r6", "r7"],
            &["vs1"],
            &AssumptionOptions::default(),
        );
        let got: Vec<u128> = lifted.run(&[vs1], 0).iter().map(int).collect();
        let want: Vec<u128> = (0..4).map(|i| vs1 >> (96 - 32 * i) & 0xffff_ffff).collect();
        assert_eq!(got, want, "{arch:?}");
    }
}

/// The scalar multiply-add CallOthers read their target, the addend or
/// multiplicand: XT for `xs*m*sp`, vrD for `xs*m*qp`.
#[test]
fn vsx_multiply_add_ops_read_their_target() {
    let sp = [
        0xf022_1808u32,
        0xf022_1848,
        0xf022_1888,
        0xf022_18c8,
        0xf022_1c08,
        0xf022_1c48,
        0xf022_1c88,
        0xf022_1cc8,
    ];
    let qp = [
        0xfc43_2308u32,
        0xfc43_2309,
        0xfc43_2348,
        0xfc43_2349,
        0xfc43_2388,
        0xfc43_2389,
        0xfc43_23c8,
        0xfc43_23c9,
    ];
    let forms = sp
        .map(|w| (w, "vs1"))
        .into_iter()
        .chain(qp.map(|w| (w, "vs34")));
    for (word, target) in forms {
        let lifted = lift_regs(
            Arch::Ppc64le,
            &[word, BLR],
            &[target],
            &[target],
            &AssumptionOptions::default(),
        );
        let (f, vn) = (&lifted.f, lifted.input(0));
        let reads = f
            .walk()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::CallOther { .. }))
            .any(|n| {
                f.node_inputs(n).into_iter().any(|v| {
                    matches!(f.node_kind(f.producer(v)),
                        NodeKind::InitialVar(id) if f.initial_vn(*id) == vn)
                })
            });
        assert!(reads, "{word:#x}: no CallOther reads the incoming {target}");
    }
}
