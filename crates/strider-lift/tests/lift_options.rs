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

/// `Default` cannot hand out a borrow of a temporary, so both tables come
/// from one `static` empty one, shared by every default.
#[test]
fn default_options_share_their_empty_tables() {
    let (a, b) = (LiftOptions::default(), LiftOptions::default());
    assert!(std::ptr::eq(a.per_address_ccs, b.per_address_ccs));
    assert!(std::ptr::eq(a.cfg.known_targets, b.cfg.known_targets));
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
        per_address_ccs: &per_address_ccs,
        ..LiftOptions::default()
    };
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
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

/// An override for a target this function never calls contributes nothing to
/// the tracked set: a caller passes a binary-wide table, and seeding every
/// entry's argument registers costs the whole table per function.
#[test]
fn an_override_this_function_never_calls_seeds_nothing() {
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
    let edi = lifter.sleigh_regs().name_to_vn("EDI").expect("EDI");
    let mut override_cc = cc.clone();
    override_cc.arg_passing_regs = vec![edi];
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    // 0x9000 is not a call target of this function.
    per_address_ccs.insert(0x9000u64, override_cc);
    let opts = LiftOptions {
        per_address_ccs: &per_address_ccs,
        ..LiftOptions::default()
    };
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;
    assert!(
        !f.all_vns()
            .iter()
            .any(|v| vn_container::vn_contains(v, &edi)),
        "EDI belongs to an uncalled override and must not be tracked, got {:?}",
        f.all_vns()
    );
}
