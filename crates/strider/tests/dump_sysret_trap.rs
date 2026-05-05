//! Exploratory: dump the pcode Sleigh emits for x86_64 sysret and
//! ARM-32 trap.  Used to verify the v2 CallOther classification table
//! during Task 5 of the plan.  Marked #[ignore] so it doesn't run
//! in the normal suite.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::{Opcode, mem_readers::BufMemReader};

#[test]
#[ignore]
fn dump_x86_64_sysret_pcode() {
    let arch = strider::SleighArch::x86_64();
    // SYSRET REX.W = 0x48 0x0F 0x07 (return-to-user 64-bit)
    // bare SYSRET = 0x0F 0x07 (return-to-user 32-bit compat)
    let bytes = vec![0x48u8, 0x0f, 0x07, 0xc3, 0xc3, 0xc3];
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh new");
    let res = sleigh.lift_one(entry).expect("lift");
    println!("=== sysret pcode ===");
    for (i, ins) in res.insns.iter().enumerate() {
        println!("  [{i}] {:?}", ins.opcode);
        if matches!(ins.opcode, Opcode::CallOther) {
            let id = ins.inputs[0].addr_off as u32;
            let name = sleigh.user_op_name(id);
            println!("       user-op id={id} name={name:?}");
        }
    }
}

#[test]
#[ignore]
fn dump_arm_trap_pcode() {
    let arch = strider::SleighArch::arm();
    // ARM-32 UDF #0 -> 0xE7F000F0 LE = F0 00 F0 E7
    // (Permanently undefined instruction; conventionally a trap.)
    let bytes = vec![0xf0u8, 0x00, 0xf0, 0xe7];
    let entry = 0x1000u64;
    let reader = BufMemReader::new(bytes, entry);
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh new");
    let res = sleigh.lift_one(entry).expect("lift");
    println!("=== arm trap (UDF #0) pcode ===");
    for (i, ins) in res.insns.iter().enumerate() {
        println!("  [{i}] {:?}", ins.opcode);
        if matches!(ins.opcode, Opcode::CallOther) {
            let id = ins.inputs[0].addr_off as u32;
            let name = sleigh.user_op_name(id);
            println!("       user-op id={id} name={name:?}");
        }
    }
}
