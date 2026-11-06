//! `xgetbv` returns XCR[ECX]: XCR0 for 0, XINUSE for 1.

use strider_ir::IRViewer;

use crate::common;

const BASE: u64 = 0x10000;

#[test]
fn xgetbv_of_two_selectors_does_not_fold_to_one_value() {
    // xor ecx, ecx; xgetbv; mov esi, eax; mov ecx, 1; xgetbv; xor eax, esi; ret
    let bytes = vec![
        0x31, 0xc9, 0x0f, 0x01, 0xd0, 0x89, 0xc6, 0xb9, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x01, 0xd0,
        0x31, 0xf0, 0xc3,
    ];
    let f = common::analyze_bytes(common::Arch::X64, bytes, BASE, None, BASE).function;
    let rax = common::returned(&f)[0];
    assert_eq!(f.int_const_u128(rax), None, "{}", f.to_text(false));
}
