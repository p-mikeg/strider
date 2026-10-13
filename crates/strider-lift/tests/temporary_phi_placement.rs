//! Phis for Sleigh temporaries are placed semi-pruned: a temporary that is
//! region-local everywhere gets none, one a split machine instruction carries
//! across a region boundary still does.

use strider_ir::IRViewer;
use strider_lift::LiftOptions;

/// The UNIQUE-space varnodes a lifted `Phi` carries.  Every node, not the
/// control walk: a phi nothing consumes is still placed.
fn temporary_phis(bytes: Vec<u8>) -> Vec<rsleigh::Vn> {
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(lifter.sleigh_regs())
        .expect("systemv");
    let opts = LiftOptions::default();
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;
    f.graph()
        .all_node_ids()
        .filter(|&node| matches!(f.node_kind(node), strider_ir::node::NodeKind::Phi))
        .filter_map(|node| f.node_outputs(node).first().copied())
        .filter_map(|v| f.get_vn_for_value(v))
        .filter(|vn| vn.addr_space == rsleigh::VnSpace::UNIQUE)
        .collect()
}

/// Sleigh reuses one unique offset per constructor, so every flag-setting
/// instruction writes the same temporaries and the join below would otherwise
/// carry a phi for each of them.
#[test]
fn a_region_local_temporary_gets_no_phi() {
    // 1000: 01 c8          add eax,ecx
    // 1002: 75 fc          jne 1000
    // 1004: c3             ret
    let phis = temporary_phis(vec![0x01, 0xc8, 0x75, 0xfc, 0xc3]);
    assert!(
        phis.is_empty(),
        "no temporary is read across a region boundary here, got {phis:?}",
    );
}

/// `bsr` is one machine instruction whose pcode loops, so its regions split
/// mid-instruction and its temporaries ARE live across the boundary.
#[test]
fn a_temporary_a_split_instruction_carries_keeps_its_phi() {
    // 1000: 0f bd c0       bsr eax,eax
    // 1003: c3             ret
    let phis = temporary_phis(vec![0x0f, 0xbd, 0xc0, 0xc3]);
    assert!(
        !phis.is_empty(),
        "a temporary live across the split of one instruction needs a phi",
    );
}
