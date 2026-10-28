use strider_ir::{IRViewer, IRWalker};

/// `test edi,edi; je 0x1009; call 0x1100; 0x1009: mov rax,[rsp]; ret`.
/// 0x1009 is a join of the branch-taken edge and the call's fall-through, so
/// the SP it loads from is a `Phi` whose call-side input is the post-call SP.
const SP_JOIN: [u8; 14] = [
    0x85, 0xff, // 1000: test edi,edi
    0x74, 0x05, // 1002: je 0x1009
    0xe8, 0xf7, 0x00, 0x00, 0x00, // 1004: call 0x1100
    0x48, 0x8b, 0x04, 0x24, // 1009: mov rax,[rsp]
    0xc3, // 100d: ret
];

/// Lifts [`SP_JOIN`] on x86-64 under SystemV, with 0x1100's `per_address_ccs`
/// override built from that same convention by `shape`.
fn lift_sp_join(
    shape: impl FnOnce(&'static rsleigh::SleighRegs, &mut strider_target::BuiltCallingConvention),
) -> anyhow::Result<strider_ir::Function> {
    let arch = strider_target::SleighArch::x86_64();
    let mut lifter = strider_lift::lift::Lifter::new(
        arch,
        rsleigh::mem_readers::BufMemReader::new(SP_JOIN.to_vec(), 0x1000),
    )?;
    let regs = lifter.sleigh_regs();
    let cc = strider_target::CallingConvention::x86_64_systemv().build(regs)?;
    let mut override_cc = cc.clone();
    shape(regs, &mut override_cc);
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    per_address_ccs.insert(0x1100u64, override_cc);
    let opts = strider_cfg::CfgOptions::default();
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts)?;
    Ok(lifter
        .build_ir_with(
            &cfg,
            cc,
            &opts,
            &per_address_ccs,
            &strider_ir::AssumptionOptions::none(),
        )?
        .function)
}

/// The constant the join's SP is the `Call`'s own SP input plus, which is the
/// callee's stack pop.  `None` when the join's SP is anything else: an opaque
/// `Call` clobber output carries no stack offset at all.
fn join_sp_pop(f: &strider_ir::Function) -> Option<u128> {
    let call = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Call { .. }))
        .next()
        .expect("a Call node");
    // `build_call`'s inputs are `[ctrl, mem, target, sp, args..]`.
    let sp_in = f.nth_input(call, 3).expect("the Call's SP input");
    let load = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Load(_)))
        .next()
        .expect("the join's load");
    // `Load`'s inputs are `[mem, address]`.
    let addr = f.nth_input(load, 1).expect("the load's address");
    let phi = f.producer(addr);
    assert!(
        matches!(f.node_kind(phi), strider_ir::node::NodeKind::Phi),
        "the join's SP must be a Phi, got {:?}",
        f.node_kind(phi),
    );
    // A `Phi`'s inputs are `[token, one per predecessor]`.
    let arms: Vec<strider_ir::node::ValueId> = f.node_inputs(phi).iter().skip(1).collect();
    arms.iter().find_map(|&arm| {
        let add = f.producer(arm);
        if !matches!(
            f.node_kind(add),
            strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)
        ) {
            return None;
        }
        let [a, b] = f.node_inputs_exact(add).ok()?;
        let other = if a == sp_in {
            b
        } else if b == sp_in {
            a
        } else {
            return None;
        };
        f.int_const_u128(other)
    })
}

/// The override's stack pop reaches `build_call`, and the post-call SP stays
/// the adjust over the `Call`'s own SP input: SP is neither a ret-val nor a
/// clobber, so no writeback races with it.
#[test]
fn an_overridden_calls_post_call_sp_is_its_own_adjust() {
    let f = lift_sp_join(|_, o| o.ret_stack_pop = 16).expect("ir");
    assert_eq!(
        join_sp_pop(&f),
        Some(16),
        "the join must read the pre-call SP plus the override's pop, got {}",
        f.to_text(false),
    );
}

/// An override naming another register as the stack pointer is refused.  It
/// passes `validate` and `check_addr_size`, which are both intra-convention,
/// yet `build_call` adjusts the FUNCTION's SP while the clobber split honours
/// the override's: the function's SP becomes a clobber and its writeback
/// replaces the adjust with the opaque `Call` output, so the join above reads
/// a value with no stack offset at all.
#[test]
fn an_override_renaming_the_stack_pointer_is_refused() {
    let r = lift_sp_join(|regs, o| {
        o.stack_vn = regs.name_to_vn("RBP").expect("RBP");
        o.callee_saved_regs.retain(|v| *v != o.stack_vn);
    });
    let Err(e) = r else {
        panic!("an override must not rename the stack pointer");
    };
    let msg = format!("{e:#}");
    assert!(
        msg.contains("stack pointer"),
        "the error must name the stack pointer, got {msg}",
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
    let mut lifter = strider_lift::lift::Lifter::new(
        arch,
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("lifter");
    let cc = strider_target::CallingConvention::x86_cdecl()
        .build(lifter.sleigh_regs())
        .expect("cdecl");
    // cdecl passes on the stack, so EDI is named by neither the convention nor
    // the fixture's two instructions.
    let edi = lifter.sleigh_regs().name_to_vn("EDI").expect("EDI");
    let mut override_cc = cc.clone();
    override_cc.arg_passing_regs = vec![edi];
    // cdecl preserves EDI, and `validate` refuses a register that is both.
    override_cc
        .callee_saved_regs
        .retain(|v| !vn_container::vn_contains(v, &edi));
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    per_address_ccs.insert(0x1006u64, override_cc);
    let opts = strider_cfg::CfgOptions::default();
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts).expect("cfg");
    let f = lifter
        .build_ir_with(
            &cfg,
            cc,
            &opts,
            &per_address_ccs,
            &strider_ir::AssumptionOptions::none(),
        )
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
    let mut lifter = strider_lift::lift::Lifter::new(
        arch,
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("lifter");
    let cc = strider_target::CallingConvention::x86_cdecl()
        .build(lifter.sleigh_regs())
        .expect("cdecl");
    let edi = lifter.sleigh_regs().name_to_vn("EDI").expect("EDI");
    let mut override_cc = cc.clone();
    override_cc.arg_passing_regs = vec![edi];
    // cdecl preserves EDI, and `validate` refuses a register that is both.
    override_cc
        .callee_saved_regs
        .retain(|v| !vn_container::vn_contains(v, &edi));
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    // 0x9000 is not a call target of this function.
    per_address_ccs.insert(0x9000u64, override_cc);
    let opts = strider_cfg::CfgOptions::default();
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts).expect("cfg");
    let f = lifter
        .build_ir_with(
            &cfg,
            cc,
            &opts,
            &per_address_ccs,
            &strider_ir::AssumptionOptions::none(),
        )
        .expect("ir")
        .function;
    assert!(
        !f.all_vns()
            .iter()
            .any(|v| vn_container::vn_contains(v, &edi)),
        "EDI belongs to an uncalled override and must not be tracked, got {:?}",
        f.all_vns()
    );
}

/// A tail call selects its target's override as a direct call does, so the
/// override's argument registers are tracked: `handle_tail_call` reads them
/// through `read_variable`, which hard-fails on an untracked varnode.
#[test]
fn a_tail_calls_override_argument_registers_are_seeded() {
    // jmp g ; ret ; g: ret, under a 2-byte bound, so the `jmp` is a tail call.
    let bytes = vec![0xeb, 0x01, 0xc3, 0xc3];
    let arch = strider_target::SleighArch::x86();
    let mut lifter = strider_lift::lift::Lifter::new(
        arch,
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("lifter");
    let cc = strider_target::CallingConvention::x86_cdecl()
        .build(lifter.sleigh_regs())
        .expect("cdecl");
    let edi = lifter.sleigh_regs().name_to_vn("EDI").expect("EDI");
    let mut override_cc = cc.clone();
    override_cc.arg_passing_regs = vec![edi];
    // cdecl preserves EDI, and `validate` refuses a register that is both.
    override_cc
        .callee_saved_regs
        .retain(|v| !vn_container::vn_contains(v, &edi));
    let mut per_address_ccs = rustc_hash::FxHashMap::default();
    per_address_ccs.insert(0x1003u64, override_cc);
    let opts = strider_cfg::CfgOptions {
        bound: strider_cfg::FunctionBound::size(2),
        ..strider_cfg::CfgOptions::default()
    };
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts).expect("cfg");
    assert!(
        cfg.regions().any(|r| matches!(
            r.terminator,
            strider_cfg::RegionTerminator::TailCall { target } if target.addr == 0x1003
        )),
        "premise: the jmp is a tail call to the overridden address"
    );
    let f = lifter
        .build_ir_with(
            &cfg,
            cc,
            &opts,
            &per_address_ccs,
            &strider_ir::AssumptionOptions::none(),
        )
        .expect("a tail call's override argument register must be tracked")
        .function;
    assert!(
        f.all_vns()
            .iter()
            .any(|v| vn_container::vn_contains(v, &edi)),
        "EDI must be tracked, got {:?}",
        f.all_vns()
    );
}
