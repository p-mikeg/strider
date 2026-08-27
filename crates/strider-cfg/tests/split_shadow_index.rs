//! A split must carry the shadow index to the half that keeps the shadower.
//!
//! `find_region_containing_addr` falls back to `shadowed_starts`, which
//! `add_region` keys on the SHADOWED region's start. `split_region` gives the
//! second half a new start, so without re-indexing the lookup answers `None`
//! for bytes that half owns and a fresh region is decoded INSIDE an
//! instruction, with nothing reported.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions};
use strider_target::SleighArch;

fn build(bytes: Vec<u8>, start: u64) -> Cfg {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, start);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
        .build()
        .expect("build")
}

fn bytes(split_variant: bool) -> Vec<u8> {
    let mut b = vec![0u8; 0x14];
    b[0x00] = 0xeb;
    b[0x01] = 0x08; // 0x1000 jmp 0x100a
    b[0x02] = 0x31;
    b[0x03] = 0xc0; // 0x1002 xor eax,eax
    b[0x04] = 0x48;
    b[0x05] = 0xb8; // 0x1004 movabs rax, imm64
    // immediate 0x1006..0x100e, with B's two bytes at 0x100a/0x100b
    b[0x0a] = 0xeb;
    b[0x0b] = 0xf6; // 0x100a jmp 0x1002
    b[0x0e] = 0x74;
    b[0x0f] = 0xfd; // 0x100e je 0x100d
    if split_variant {
        b[0x10] = 0xeb;
        b[0x11] = 0xf2; // 0x1010 jmp 0x1004
    } else {
        b[0x10] = 0xc3; // 0x1010 ret
    }
    b[0x12] = 0xc3;
    b
}

#[test]
fn a_split_keeps_the_shadowed_half_reachable() {
    // Control: region A (0x1002) hides B (0x100a) inside a `movabs` immediate,
    // and the interior target 0x100d is recognised as A's, not decoded.
    let no_split = build(bytes(false), 0x1000);
    assert!(
        !no_split
            .regions()
            .any(|r| r.start_addr.machine_addr.addr == 0x100d),
        "control: 0x100d is owned by region A, so no region may start there"
    );

    // Subject: the same graph plus an edge that splits A at 0x1004.
    let split = build(bytes(true), 0x1000);
    assert!(
        !split
            .regions()
            .any(|r| r.start_addr.machine_addr.addr == 0x100d),
        "after the split, 0x100d is owned by the second half; a region starting \
         there means the lookup missed it and decoded inside the movabs immediate"
    );
}
