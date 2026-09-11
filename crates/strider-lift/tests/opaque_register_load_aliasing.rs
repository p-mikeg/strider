//! A named register write does not advance the memory chain, so an opaque
//! REGISTER-space LOAD after one must not forward across it: two loads at the
//! same address either side of `mtsr sr0,r5` would dedup into one value, and
//! the IR would claim the two reads are equal when the machine does not.

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

/// ppc32be. `mfsrin` reads the REGISTER space at `SEG_REGISTER_BASE + (r4>>28)`,
/// which folds to no declared register, so both reads take the opaque path;
/// `mtsr` writes the segment register `sr0` by name in between.
const BYTES: [u8; 16] = [
    0x7c, 0x60, 0x25, 0x26, // mfsrin r3,r4
    0x7c, 0xa0, 0x01, 0xa4, // mtsr   sr0,r5
    0x7c, 0xc0, 0x25, 0x26, // mfsrin r6,r4
    0x4e, 0x80, 0x00, 0x20, // blr
];

#[test]
fn an_opaque_register_load_does_not_forward_across_a_named_register_write() {
    let arch = strider_target::SleighArch::ppc32be();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(BYTES.to_vec(), 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let sr0 = lifter.sleigh_regs().name_to_vn("sr0").expect("sr0");
    let cc = strider_target::CallingConvention::powerpc_sysv32()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let opts = strider_lift::LiftOptions::default();
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, &opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;

    let is_register = |id, space_of: fn(&NodeKind) -> Option<rsleigh::VnSpace>| {
        space_of(f.node_kind(id)) == Some(rsleigh::VnSpace::REGISTER)
    };
    let opaque_loads: Vec<_> = f
        .graph()
        .all_node_ids()
        .filter(|&id| {
            is_register(id, |k| match k {
                NodeKind::Load(space) => Some(*space),
                _ => None,
            }) && f.int_const_u128(f.load_addr(id)).is_none()
        })
        .collect();
    assert_eq!(
        opaque_loads.len(),
        2,
        "each `mfsrin` reads the space at its own program point",
    );

    // The named write reaches the space: `sr0`'s slot is stored before the
    // second read, so the load is over a chain that has seen it.
    let sr0_stores = f
        .graph()
        .all_node_ids()
        .filter(|&id| {
            is_register(id, |k| match k {
                NodeKind::Store(space) => Some(*space),
                _ => None,
            }) && f.int_const_u128(f.store_addr(id)) == Some(u128::from(sr0.addr_off))
        })
        .count();
    assert!(sr0_stores > 0, "no named register write reaches the space");
}
