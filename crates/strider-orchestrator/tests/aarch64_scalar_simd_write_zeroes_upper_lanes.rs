//! Writing a scalar B/H/S/D register zeroes the rest of its vector register:
//! after `movi v0.2d, #-1; <scalar write to v0>`, lane `v0.d[1]` reads 0.

mod common;

use common::words_image;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::Endianness;

const BASE: u64 = 0x1000;

/// `movi v0.2d, #-1; <insn>; mov x0, v0.d[1]; ret`
fn upper_lane_after(arch: common::Arch, insn: u32) -> Option<u128> {
    let words = [0x6f07_e7e0, insn, 0x4e18_3c00, 0xd65f_03c0];
    let bytes = words_image(&words, Endianness::Little);
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
fn a_scalar_simd_write_zeroes_the_upper_lanes() {
    let mut wrong = Vec::new();
    for arch in [common::Arch::Aarch64, common::Arch::Aarch64Be] {
        for (name, insn) in [
            ("fmadd d0, d1, d2, d3", 0x1f42_0c20),
            ("uaddlv h0, v1.16b", 0x6e30_3820),
            ("umaxv b0, v1.16b", 0x6e30_a820),
            ("fmaxv s0, v1.4s", 0x6e30_f820),
        ] {
            let got = upper_lane_after(arch, insn);
            if got != Some(0) {
                wrong.push(format!("{arch:?} {name}: {got:x?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
