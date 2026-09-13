//! A sign-extending load into a W register sign-extends to 32 bits and clears
//! the upper half of the X register (Arm ARM `LDRSB`/`LDRSH`/`LDURSB`/`LDURSH`/
//! `LDTRSB`/`LDTRSH`, `opc == 11`: `regsize = 32`).

mod common;

use common::words_image;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::Endianness;

const BASE: u64 = 0x1000;

const STRB_W9_SP_8: u32 = 0x3900_23e9;
const STRH_W9_SP_8: u32 = 0x7900_13e9;

/// `sub sp, sp, #16; mov w9, #v; str w9, [sp, #8]; add x1, sp, #8; mov x2, #0;
/// <load>; add sp, sp, #16; ret`
fn function(mov_w9: u32, store: u32, load: u32) -> Vec<u32> {
    vec![
        0xd100_43ff,
        mov_w9,
        store,
        0x9100_23e1,
        0xd280_0002,
        load,
        0x9100_43ff,
        0xd65f_03c0,
    ]
}

fn returned_x0(arch: common::Arch, words: &[u32]) -> Option<u128> {
    // AArch64 instructions are little-endian on both data endiannesses.
    let bytes = words_image(words, Endianness::Little);
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
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
fn a_w_register_sign_extending_load_zeroes_the_upper_half() {
    let byte = (0x5280_1009, STRB_W9_SP_8, 0xffff_ff80u128);
    let half = (0x5290_0009, STRH_W9_SP_8, 0xffff_8000u128);
    let cases = [
        ("ldrsb w0, [x1]", 0x39c0_0020, byte),
        ("ldursb w0, [x1]", 0x38c0_0020, byte),
        ("ldtrsb w0, [x1]", 0x38c0_0820, byte),
        ("ldrsb w0, [x1], #1", 0x38c0_1420, byte),
        ("ldrsb w0, [x1, #0]!", 0x38c0_0c20, byte),
        ("ldrsb w0, [x1, x2]", 0x38e2_6820, byte),
        ("ldrsh w0, [x1]", 0x79c0_0020, half),
        ("ldursh w0, [x1]", 0x78c0_0020, half),
        ("ldtrsh w0, [x1]", 0x78c0_0820, half),
        ("ldrsh w0, [x1], #1", 0x78c0_1420, half),
        ("ldrsh w0, [x1, #0]!", 0x78c0_0c20, half),
        ("ldrsh w0, [x1, x2]", 0x78e2_6820, half),
    ];
    let mut wrong = Vec::new();
    for arch in [common::Arch::Aarch64, common::Arch::Aarch64Be] {
        for (name, load, (mov, store, expected)) in cases {
            let got = returned_x0(arch, &function(mov, store, load));
            if got != Some(expected) {
                wrong.push(format!("{arch:?} {name}: {got:x?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn an_x_register_sign_extending_load_fills_the_upper_half() {
    for arch in [common::Arch::Aarch64, common::Arch::Aarch64Be] {
        for (name, load) in [
            ("ldrsb x0, [x1]", 0x3980_0020),
            ("ldursb x0, [x1]", 0x3880_0020),
        ] {
            assert_eq!(
                returned_x0(arch, &function(0x5280_1009, STRB_W9_SP_8, load)),
                Some(0xffff_ffff_ffff_ff80),
                "{arch:?} {name}",
            );
        }
    }
}
