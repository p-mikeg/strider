//! An AltiVec vector result reaches `Return` on every PowerPC userland ABI.
//! With no vector register in the convention nothing roots `v2`, so DCE
//! deletes the whole computation and the function lifts as "return the
//! incoming r3".  The kernel flavours name no vector register, which is the
//! other half of the same contract: a vector-free function tracks none.

mod common;

use common::{Arch, words_image};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_target::CallingConvention;

const BASE: u64 = 0x1000;

/// `vadduwm v2,v2,v3; blr`: four packed 32-bit adds of the two incoming
/// vector arguments, returned in `v2`.
const WORDS: [u32; 2] = [0x1042_1880, 0x4e80_0020];

/// `add r3,r3,r4; blr`: no vector register anywhere in the function.
const SCALAR_WORDS: [u32; 2] = [0x7c63_2214, 0x4e80_0020];

/// Every PowerPC arch with its userland and kernel convention.
fn ppc_abis() -> [(Arch, CallingConvention, CallingConvention); 4] {
    [
        (
            Arch::Ppc64le,
            CallingConvention::powerpc64_elf_v2(),
            CallingConvention::powerpc64_elf_v2_linux_kernel(),
        ),
        (
            Arch::Ppc64be,
            CallingConvention::powerpc64_elf_v1(),
            CallingConvention::powerpc64_elf_v1_linux_kernel(),
        ),
        (
            Arch::Ppc32be,
            CallingConvention::powerpc_sysv32(),
            CallingConvention::powerpc_sysv32_linux_kernel(),
        ),
        (
            Arch::Ppc32le,
            CallingConvention::powerpc_sysv32(),
            CallingConvention::powerpc_sysv32_linux_kernel(),
        ),
    ]
}

#[test]
fn a_vector_add_survives_to_the_return() {
    for (arch, cc, _) in ppc_abis() {
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

/// A function that touches no vector register tracks none under the kernel
/// convention, and every one the userland convention names under that: a
/// 16-byte tracked varnode is an `InitialVar` plus a `Call` argument and a
/// `Return` input per register, and it renumbers every collected stack
/// argument's `arg(n)`.
#[test]
fn a_scalar_function_tracks_vector_registers_only_under_the_userland_abi() {
    for (arch, user_cc, kernel_cc) in ppc_abis() {
        let tracked_vectors = |cc: CallingConvention| {
            let bytes = words_image(&SCALAR_WORDS, arch.sleigh().endianness());
            let (mut strider, _) = common::strider_over_bytes(arch, bytes, BASE, None);
            let cc = cc.build(strider.sleigh_regs()).expect("build cc");
            let f = strider
                .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
                .unwrap_or_else(|e| panic!("{arch:?}: {e}"))
                .function;
            let vectors = f.all_vns().iter().filter(|vn| vn.size == 16).count();
            (vectors, f.to_text(false))
        };
        let (under_user, _) = tracked_vectors(user_cc);
        assert_eq!(under_user, 12, "{arch:?}: userland v2-v13");
        let (under_kernel, text) = tracked_vectors(kernel_cc);
        assert_eq!(
            under_kernel, 0,
            "{arch:?}: the kernel convention names no vector register:\n{text}",
        );
    }
}
