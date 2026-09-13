//! `lhbrx`/`lwbrx` load with the bytes reversed from the machine's own order,
//! and `sthbrx`/`stwbrx` store that way, on a little-endian machine too.

mod common;

use common::{Arch, words_image};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;

/// `lis r9, 0x1122; ori r9, r9, 0x3344; li r10, -8`
const PRELUDE: [u32; 3] = [0x3d20_1122, 0x6129_3344, 0x3940_fff8];
/// `li r10, DATA`
const LI_R10_DATA: u32 = 0x3940_1100;
const DATA: u64 = 0x1100;
/// `lwbrx r3, 0, r10` / `lhbrx r3, 0, r10`
const LWBRX: u32 = 0x7c60_542c;
const LHBRX: u32 = 0x7c60_562c;
const STWBRX: u32 = 0x7d21_552c;
const STHBRX: u32 = 0x7d21_572c;
const LWZ: u32 = 0x8061_fff8;
const LHZ: u32 = 0xa061_fff8;
const BLR: u32 = 0x4e80_0020;

fn returned_r3(arch: Arch, body: &[u32]) -> Option<u128> {
    let mut words = PRELUDE.to_vec();
    words.extend(body);
    words.push(BLR);
    let mut bytes = words_image(&words, arch.sleigh().endianness());
    bytes.resize(usize::try_from(DATA - BASE).unwrap(), 0);
    bytes.extend([0x11, 0x22, 0x33, 0x44]);
    let rom = strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes.clone());
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, Some(Box::new(rom)));
    let f = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.int_const_u128(f.node_inputs(ret)[2])
}

#[test]
fn a_byte_reversed_load_reverses_the_machine_byte_order() {
    let mut wrong = Vec::new();
    for (arch, word, half) in [
        (Arch::Ppc32be, 0x4433_2211, 0x2211),
        (Arch::Ppc32le, 0x1122_3344, 0x1122),
        (Arch::Ppc64be, 0x4433_2211, 0x2211),
        (Arch::Ppc64le, 0x1122_3344, 0x1122),
    ] {
        for (name, load, expected) in [("lwbrx", LWBRX, word), ("lhbrx", LHBRX, half)] {
            let got = returned_r3(arch, &[LI_R10_DATA, load]);
            if got != Some(expected) {
                wrong.push(format!("{arch:?} {name}: {got:x?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `stwbrx`/`sthbrx r9, r1, r10` of 0x11223344, read back by `lwz`/`lhz -8(r1)`.
#[test]
fn a_byte_reversed_store_reverses_the_machine_byte_order() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le] {
        for (name, store, load, expected) in [
            ("stwbrx; lwz", STWBRX, LWZ, 0x4433_2211),
            ("sthbrx; lhz", STHBRX, LHZ, 0x4433),
        ] {
            assert_eq!(
                returned_r3(arch, &[store, load]),
                Some(expected),
                "{arch:?} {name}"
            );
        }
    }
}
