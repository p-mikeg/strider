//! A MIPS delay slot decodes in its branch's ISA mode on a reused lift engine,
//! whatever an earlier function committed at the slot's own address.

use rsleigh::mem_readers::BufMemReader;
use strider_lift::lift::Lifter;
use strider_target::SleighArch;

/// fixtures/out/mips32be/control.elf `abs_val`:
///
/// ```text
/// 4007d0  bltz a0, 0x4007e4
/// 4007d4  move v0, a0        ; slot
/// 4007d8  jr ra
/// 4007dc  nop
/// 4007e0  jr ra
/// 4007e4  negu v0, a0        ; slot
/// ```
const ABS_VAL: [u8; 24] = [
    0x04, 0x80, 0x00, 0x03, 0x00, 0x80, 0x10, 0x25, 0x03, 0xe0, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00,
    0x03, 0xe0, 0x00, 0x08, 0x00, 0x04, 0x10, 0x23,
];
const BASE: u64 = 0x4007d0;

fn lifter() -> Lifter<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::mipsbe32();
    let mut bytes = ABS_VAL.to_vec();
    bytes.resize(0x40, 0);
    Lifter::new(
        arch,
        rsleigh::Sleigh::new(
            arch.sla_spec(),
            arch.pspec(),
            BufMemReader::new(bytes, BASE),
        )
        .expect("create Sleigh"),
    )
    .unwrap()
}

fn insns(lifter: &mut Lifter<BufMemReader<Vec<u8>>>, entry: u64, size: u64) -> Vec<String> {
    let opts = strider_cfg::CfgOptions {
        fn_max_size: Some(size),
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(entry.into(), &opts, &Default::default())
        .expect("build_cfg");
    let mut out: Vec<String> = cfg
        .regions()
        .flat_map(|r| r.insns.iter())
        .map(|i| format!("{:?} len={} {:?}", i.addr, i.len, i.insn))
        .collect();
    out.sort();
    out
}

#[test]
fn a_mips16_function_at_a_slot_address_does_not_change_the_branch_owning_it() {
    let alone = insns(&mut lifter(), BASE, 24);
    let mut reused = lifter();
    let opts = strider_cfg::CfgOptions {
        fn_max_size: Some(0x20),
        ..Default::default()
    };
    let _ = reused.build_cfg((BASE + 5).into(), &opts, &Default::default());
    assert_eq!(insns(&mut reused, BASE, 24), alone);
}
