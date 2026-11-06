//! AArch64 `ldr h21` zeroes bytes 2..16 of v21 through a 6-byte copy at
//! `h21 + 2`, which partially overlaps `s21`. With no `d21` / `q21` named
//! anywhere, both views must still share one tracked variable, or the zeroing
//! never reaches a later read of `s21`.

use strider_ir::UBig;
use strider_ir_test_utils::MockRom;
use strider_ir_test_utils::interp::{self, EntryVar, Env, Outcome, Slot};

const CODE: [u8; 16] = [
    0x35, 0x00, 0x40, 0xbd, // ldr  s21, [x1]
    0x15, 0x00, 0x40, 0x7d, // ldr  h21, [x0]
    0xa0, 0x02, 0x26, 0x1e, // fmov w0, s21
    0xc0, 0x03, 0x5f, 0xd6, // ret
];

#[test]
fn a_narrow_write_reaches_a_partially_overlapping_view() {
    let arch = strider_target::SleighArch::aarch64();
    let mut lifter = strider_lift::Lifter::new(
        arch,
        rsleigh::mem_readers::BufMemReader::new(CODE.to_vec(), 0x1000),
    )
    .expect("lifter");
    let regs = lifter.sleigh_regs();
    let [x0, x1, sp] = ["x0", "x1", "sp"].map(|r| regs.name_to_vn(r).expect("register"));
    let cc = strider_target::CallingConvention::aarch64_aapcs64()
        .build(regs)
        .expect("cc");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let f = lifter.build_ir(&cfg, cc).expect("ir").function;
    strider_ir::validate::validate(&f).expect("valid IR");

    // h21 reads 0x5678 from x0; s21 first reads 0x1234_abcd from x1.
    let rom = MockRom::regions([
        (0x100, vec![0x78, 0x56]),
        (0x200, vec![0xcd, 0xab, 0x34, 0x12]),
    ]);
    let entry = |e: EntryVar| match e.vn {
        v if v == x0 => UBig::from(0x100u32),
        v if v == x1 => UBig::from(0x200u32),
        v if v == sp => UBig::from(0x8000u32),
        _ => UBig::ZERO,
    };
    let env = Env {
        entry: &entry,
        rom: Some(&rom),
        seed: 0,
        step_budget: 16,
        private: None,
        keeps: None,
    };
    let slot = f
        .ret_val_regs()
        .iter()
        .position(|&v| v == x0)
        .expect("x0 is returned");
    let Ok(Outcome::Return { values, .. }) = interp::run(&f, &env) else {
        panic!("the function returns");
    };
    assert_eq!(values[slot], Slot::Known(UBig::from(0x5678u32)));
}
