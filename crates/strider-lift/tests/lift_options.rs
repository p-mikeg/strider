use strider_lift::LiftOptions;

#[test]
fn lift_options_default() {
    let d = LiftOptions::default();
    assert_eq!(d.cfg.fn_max_size, None);
    assert!(!d.cfg.allow_code_before_start_addr);
    assert!(d.cfg.known_targets.is_empty());
    assert!(d.per_address_ccs.is_empty());
    assert!(d.compact);
}

#[test]
fn lift_options_embeds_cfg_knobs() {
    let opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(0x1000),
            allow_code_before_start_addr: true,
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    assert_eq!(opts.cfg.fn_max_size, Some(0x1000));
    assert!(opts.cfg.allow_code_before_start_addr);
}

#[test]
fn lift_options_embeds_known_targets_seed() {
    // `Strider::analyze` clones this map to seed its resolution loop; the raw
    // lift path passes it to the CFG builder verbatim.
    use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};

    let site = PcodeInsnAddr {
        machine_addr: MachineInsnAddr::from(0x1000u64),
        insn_index: 0,
    };
    let mut known = rustc_hash::FxHashMap::default();
    known.insert(site, ResolvedTargets::Single(0x2000.into()));
    let opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    assert_eq!(opts.cfg.known_targets.len(), 1);
    assert_eq!(
        opts.cfg.known_targets.get(&site),
        Some(&ResolvedTargets::Single(0x2000.into()))
    );
}

/// A `per_address_ccs` override names registers the lifted function itself may
/// never touch.  Its INTEGER argument registers are read through
/// `read_variable`, which hard-fails on an untracked varnode, so they must be
/// seeded into the tracked universe alongside the float ones.
#[test]
fn per_address_cc_integer_arg_registers_are_seeded() {
    // call g ; ret ; g: ret
    let bytes = vec![0xe8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0xc3];
    let arch = strider_target::SleighArch::x86();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cc = strider_target::CallingConvention::x86_cdecl()
        .build(lifter.sleigh_regs())
        .expect("cdecl");
    // cdecl passes on the stack, so EDI is named by neither the convention nor
    // the fixture's two instructions.
    let edi = lifter.sleigh_regs().name_to_vn("EDI").expect("EDI");
    let mut override_cc = cc.clone();
    override_cc.arg_passing_regs = vec![edi];
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    per_address_ccs.insert(0x1006u64, override_cc);
    let opts = LiftOptions {
        per_address_ccs,
        ..LiftOptions::default()
    };
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, &opts.per_address_ccs)
        .expect("cfg");
    let f = lifter
        .build_ir_with(&cfg, cc, &opts)
        .expect("an override's integer argument register must be tracked")
        .function;
    assert!(
        f.all_vns()
            .iter()
            .any(|v| vn_container::vn_contains(v, &edi)),
        "EDI must be tracked, got {:?}",
        f.all_vns()
    );
}
