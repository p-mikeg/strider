//! A `Call`'s outputs are the convention's registers, never a Sleigh
//! temporary: a p-code CALL writes no UNIQUE varnode, and an output per
//! tracked temporary makes every call region a def site for all of them.

use strider_ir::{IRViewer, IRWalker};
use strider_lift::LiftOptions;

#[test]
fn call_outputs_name_no_unique_temporary() {
    // 1000: add eax,ecx   (x86 flag semantics leave UNIQUE temporaries)
    // 1002: e8 01 ..      call 1008
    // 1007: c3            ret
    // 1008: c3            ret
    let bytes = vec![0x01, 0xc8, 0xe8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0xc3];
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(lifter.sleigh_regs())
        .expect("systemv");
    let opts = LiftOptions::default();
    let mut lifter = lifter;
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;

    assert!(
        f.all_vns()
            .iter()
            .any(|v| v.addr_space == rsleigh::VnSpace::UNIQUE),
        "the fixture must track at least one temporary, got {:?}",
        f.all_vns()
    );
    let call = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Call { .. }))
        .next()
        .expect("a Call node");
    // Past the structural `[Control, Memory]` pair, every output carries the
    // varnode it writes back.
    let outs: Vec<rsleigh::Vn> = f
        .node_outputs(call)
        .iter()
        .skip(2)
        .filter_map(|&v| f.get_vn_for_value(v))
        .collect();
    assert!(!outs.is_empty(), "SysV clobbers registers");
    for vn in outs {
        assert_eq!(
            vn.addr_space,
            rsleigh::VnSpace::REGISTER,
            "{vn:?} is a call output but not a register",
        );
    }
}
