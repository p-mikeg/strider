//! An AltiVec vector result reaches `Return` on every PowerPC ABI.  With no
//! vector register in the convention nothing roots `v2`, so DCE deletes the
//! whole computation and the function lifts as "return the incoming r3".

mod common;

use common::{Arch, words_image};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

const BASE: u64 = 0x1000;

/// `vadduwm v2,v2,v3; blr`: four packed 32-bit adds of the two incoming
/// vector arguments, returned in `v2`.
const WORDS: [u32; 2] = [0x1042_1880, 0x4e80_0020];

#[test]
fn a_vector_add_survives_to_the_return() {
    for (arch, cc) in [
        (
            Arch::Ppc64le,
            strider_target::CallingConvention::powerpc64_elf_v2(),
        ),
        (
            Arch::Ppc64be,
            strider_target::CallingConvention::powerpc64_elf_v1(),
        ),
        (
            Arch::Ppc32be,
            strider_target::CallingConvention::powerpc_sysv32(),
        ),
        (
            Arch::Ppc32le,
            strider_target::CallingConvention::powerpc_sysv32(),
        ),
    ] {
        let bytes = words_image(&WORDS, arch.sleigh().endianness());
        let (mut strider, _) = common::strider_over_bytes(arch, bytes, BASE, None);
        let cc = cc.build(strider.sleigh_regs()).expect("build cc");
        let f = strider
            .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
            .unwrap_or_else(|e| panic!("{arch:?}: {e}"))
            .function;
        let adds = f
            .walk()
            .filter(|&n| {
                matches!(
                    f.node_kind(n),
                    NodeKind::IntBinaryOp(strider_ir::node::IntBinaryOp::Add)
                )
            })
            .count();
        assert!(
            adds > 0,
            "{arch:?}: the vector add is unrooted and was deleted:\n{}",
            f.to_text(false)
        );
    }
}
