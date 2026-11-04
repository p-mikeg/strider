//! `ldpsw`, the acquire / release forms and the LSE atomics read and write
//! exactly their access size, and a W destination clears the upper half
//! (Arm ARM `LDPSW`, `STLRB`, `STLRH`, `LDAR`, `LDADD`, `SWP`).

use crate::common;

use crate::common::cone::{self, Val};
use crate::common::words_image;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::Endianness;

const BASE: u64 = 0x1000;

const SUB_SP_16: u32 = 0xd100_43ff;
const ADD_SP_16: u32 = 0x9100_43ff;
const RET: u32 = 0xd65f_03c0;
/// `mov w9, #0x2211; movk w9, #0x4433, lsl #16; str w9, [sp, #8]`
const STORE_44332211: [u32; 3] = [0x5284_4229, 0x72a8_8669, 0xb900_0be9];
/// `mov w10, #0x8001; movk w10, #0x9000, lsl #16; str w10, [sp, #12]`
const STORE_90008001: [u32; 3] = [0x5290_002a, 0x72b2_000a, 0xb900_0fea];
const ADD_X1_SP_8: u32 = 0x9100_23e1;

/// The returned x0, evaluated; it must not depend on an initial register.
fn returned_x0(arch: common::Arch, words: &[u32]) -> Option<u128> {
    // AArch64 instructions are little-endian on both data endiannesses.
    let bytes = words_image(words, Endianness::Little);
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let f = strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    let x0 = f.node_inputs(ret)[2];
    let [zeros, ones] = [0, u128::MAX].map(|init| match cone::eval(&f, x0, init) {
        Val::Int(v) => Some(v),
        _ => None,
    });
    (zeros == ones).then_some(zeros).flatten()
}

/// `sub sp, sp, #16; <body>; add sp, sp, #16; ret`
fn function(body: &[u32]) -> Vec<u32> {
    [&[SUB_SP_16][..], body, &[ADD_SP_16, RET]].concat()
}

#[test]
fn widths_match_the_architecture() {
    let words = [STORE_44332211, STORE_90008001].concat();
    let cases = [
        // ldpsw x0, x1, [sp, #8]; mov x0, x1
        (
            "ldpsw",
            [&words[..], &[0x6941_07e0, 0xaa01_03e0]].concat(),
            [0xffff_ffff_9000_8001u128; 2],
        ),
        // mov w10, #0xccdd; stlrb w10, [x1]; ldr w0, [sp, #8]
        (
            "stlrb",
            [
                &words[..],
                &[ADD_X1_SP_8, 0x5299_9baa, 0x089f_fc2a, 0xb940_0be0],
            ]
            .concat(),
            [0x4433_22dd, 0xdd33_2211],
        ),
        // mov w10, #0xccdd; stlrh w10, [x1]; ldr w0, [sp, #8]
        (
            "stlrh",
            [
                &words[..],
                &[ADD_X1_SP_8, 0x5299_9baa, 0x489f_fc2a, 0xb940_0be0],
            ]
            .concat(),
            [0x4433_ccdd, 0xccdd_2211],
        ),
        // ldar w0, [x1]
        (
            "ldar w",
            [&words[..], &[ADD_X1_SP_8, 0x88df_fc20]].concat(),
            [0x4433_2211; 2],
        ),
        // mov x2, #-1; ldadd w9, w2, [x1]; mov x0, x2
        (
            "ldadd w",
            [
                &words[..],
                &[ADD_X1_SP_8, 0x9280_0002, 0xb829_0022, 0xaa02_03e0],
            ]
            .concat(),
            [0x4433_2211; 2],
        ),
        // mov x2, #-1; swp w9, w2, [x1]; mov x0, x2
        (
            "swp w",
            [
                &words[..],
                &[ADD_X1_SP_8, 0x9280_0002, 0xb829_8022, 0xaa02_03e0],
            ]
            .concat(),
            [0x4433_2211; 2],
        ),
    ];
    let mut wrong = Vec::new();
    // The stored byte or half lands at the word's lowest address: its low end
    // little-endian, its high end big-endian.
    for (k, arch) in [common::Arch::Aarch64, common::Arch::Aarch64Be]
        .into_iter()
        .enumerate()
    {
        for (name, body, expected) in &cases {
            let got = returned_x0(arch, &function(body));
            if got != Some(expected[k]) {
                wrong.push(format!("{arch:?} {name}: {got:x?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// By-element `usdot` reads its lane through `SIMD_PIECE`, a pure op.
#[test]
fn by_element_usdot_lifts() {
    // usdot v0.4s, v1.16b, v2.4b[3]; ret
    let bytes = words_image(&[0x4fa2_f820, RET], Endianness::Little);
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::Aarch64, bytes, BASE, None);
    strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze");
}
