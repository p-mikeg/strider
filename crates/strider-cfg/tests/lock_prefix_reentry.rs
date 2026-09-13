//! An x86 branch to an address one byte into a `lock`-prefixed instruction
//! re-enters the same instruction's decode. The edge is exact, not a
//! stepped-over overlap, so it is not reported as interior.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, CfgOptions};
use strider_target::SleighArch;

#[test]
fn a_conditional_branch_over_a_lock_prefix_is_not_interior() {
    // 0x1000 je 0x1004         (74 02)
    // 0x1002 xor eax,eax       (31 c0)  fall-through
    // 0x1004 lock xadd %ecx,(%eax)  (f0 0f c1 08) -- je's target is one byte in
    // Both paths reach 0x1004; the je jumps to 0x1005, the bare xadd.
    // 0x1008 ret               (c3)
    // je targets 0x1005 (74 03): inside the lock xadd at 0x1004.
    let bytes = vec![
        0x74, 0x03, // 0x1000 je 0x1005
        0x31, 0xc0, // 0x1002 xor eax,eax
        0xf0, 0x0f, 0xc1, 0x08, // 0x1004 lock xadd %ecx,(%eax)
        0xc3, // 0x1008 ret
    ];
    let arch = SleighArch::x86();
    let mut sleigh = Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let cfg = Builder::for_arch(&arch, &mut sleigh, 0x1000, &CfgOptions::default())
        .build()
        .expect("build");
    assert!(
        cfg.interior_branch_targets().is_empty(),
        "a lock-prefix re-entry is exact: {:?}",
        cfg.interior_branch_targets()
    );
}

#[test]
fn a_branch_into_a_real_instruction_interior_is_still_reported() {
    // 0x1000 je 0x1006 (74 04) into the middle of movabs at 0x1002, which is
    // not a prefix re-entry.
    // 0x1002 movabs rax, imm64 -- but x86 (32-bit) has no movabs; use a 5-byte
    // mov eax, imm32 (b8 ..) and target its interior.
    let bytes = vec![
        0x74, 0x03, // 0x1000 je 0x1005
        0x31, 0xc9, // 0x1002 xor ecx,ecx
        0xb8, 0x11, 0x22, 0x33, 0x44, // 0x1004 mov eax, 0x44332211 (target 0x1005 interior)
        0xc3, // 0x1009 ret
    ];
    let arch = SleighArch::x86();
    let mut sleigh = Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let cfg = Builder::for_arch(&arch, &mut sleigh, 0x1000, &CfgOptions::default())
        .build()
        .expect("build");
    assert_eq!(
        cfg.interior_branch_targets()
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![0x1005],
        "a branch into a non-prefix instruction interior is a real overlap"
    );
}
