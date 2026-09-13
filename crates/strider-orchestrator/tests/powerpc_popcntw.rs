//! `popcntw` (Power ISA 2.06) decodes on 32-bit PowerPC as well as 64-bit.

mod common;

use common::{Arch, words_image};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;

/// `lis r4, 0x0f0f; ori r4, r4, 3; popcntw r3, r4; blr`
const WORDS: [u32; 4] = [0x3c80_0f0f, 0x6084_0003, 0x7c83_02f4, 0x4e80_0020];

#[test]
fn popcntw_counts_the_ones_in_a_word() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le] {
        let bytes = words_image(&WORDS, arch.sleigh().endianness());
        let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
        let f = strider
            .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
            .unwrap_or_else(|e| panic!("{arch:?}: {e}"))
            .function;
        let ret = f
            .walk()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
            .expect("one Return");
        assert_eq!(
            f.int_const_u128(f.node_inputs(ret)[2]),
            Some(10),
            "{arch:?}"
        );
    }
}
