//! An opaque register-space STORE re-reads every tracked register out of the
//! REGISTER space. The slot address of each re-read is a constant at the
//! SPACE's address width. At the register's OWN width the constant is masked
//! to that width, so `cr0` (0x900) and `xer_so` (0x400) both address slot 0,
//! dedup collapses their two `Load`s into one node, and the IR asserts
//! `cr0 == xer_so`.

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

/// ppc32be. `addic. r3,r3,1` writes cr0, `addc r5,r5,r6` writes the xer bits,
/// so both are tracked by the time `mtsrin r3,r4` stores through a
/// register-derived address that cannot fold to a register name.
const BYTES: [u8; 16] = [
    0x34, 0x63, 0x00, 0x01, // addic. r3,r3,1
    0x7c, 0xa5, 0x30, 0x14, // addc   r5,r5,r6
    0x7c, 0x60, 0x21, 0xe4, // mtsrin r3,r4
    0x4e, 0x80, 0x00, 0x20, // blr
];

#[test]
fn opaque_register_store_gives_colliding_registers_distinct_slots() {
    let arch = strider_target::SleighArch::ppc32be();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(BYTES.to_vec(), 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cr0 = lifter.sleigh_regs().name_to_vn("cr0").expect("cr0");
    let xer_so = lifter.sleigh_regs().name_to_vn("xer_so").expect("xer_so");
    assert_eq!(cr0.size, 1);
    assert_eq!(xer_so.size, 1);
    assert_eq!(
        cr0.addr_off & 0xff,
        xer_so.addr_off & 0xff,
        "the fixture needs two 1-byte registers colliding modulo their own width"
    );

    let cc = strider_target::CallingConvention::powerpc_sysv32()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let opts = strider_lift::LiftOptions::default();
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, &opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;

    let slot_of = |off: u64| {
        f.graph().all_node_ids().find(|&id| {
            matches!(f.node_kind(id), NodeKind::Load(sp) if *sp == rsleigh::VnSpace::REGISTER)
                && f.int_const_u128(f.load_addr(id)) == Some(u128::from(off))
                && f.value_type(f.node_outputs(id)[0])
                    .expect("load output type")
                    == strider_ir::ValueType::I8
        })
    };
    let cr0_load = slot_of(cr0.addr_off).expect("cr0 re-read at its full offset");
    let so_load = slot_of(xer_so.addr_off).expect("xer_so re-read at its full offset");
    assert_ne!(cr0_load, so_load, "cr0 and xer_so must not share a Load");
}
