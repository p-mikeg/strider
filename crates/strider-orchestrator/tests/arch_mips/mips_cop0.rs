//! `mtc0` writes a CP0 register, so a later `mfc0` of it reads the write.

use crate::common;
use crate::common::Arch;
use strider_ir::IRViewer;

/// `mfc0 v0, $12; mtc0 a0, $12; mfc0 v1, $12; jr ra; subu v0, v0, v1`
#[test]
fn mfc0_after_mtc0_reads_the_write() {
    let words = [
        0x4002_6000,
        0x4084_6000,
        0x4003_6000,
        0x03e0_0008,
        0x0043_1023,
    ];
    for arch in [
        Arch::Mips32be,
        Arch::Mips32le,
        Arch::Mips64be,
        Arch::Mips64le,
    ] {
        let bytes = common::words_image(&words, arch.sleigh().endianness());
        let (mut strider, cc) = common::strider_over_bytes(arch, bytes, 0x1000, None);
        let f = strider
            .analyze(0x1000, &cc, &Default::default(), None)
            .unwrap_or_else(|e| panic!("{arch:?}: {e}"))
            .function;
        let v0 = common::returned(&f)[0];
        assert_eq!(
            f.int_const_u128(v0),
            None,
            "{arch:?}: the two reads are one value"
        );
    }
}
