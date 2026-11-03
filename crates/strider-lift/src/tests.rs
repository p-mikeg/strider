/// AArch64 `ret` lifts as `pc = x30; return [pc];`, a p-code `RETURN`, so
/// the cfg marks it `RegionTerminator::Return`: it must route through
/// `handle_sleigh_return` and leave `unresolved_branches` empty.
#[test]
fn aarch64_ret_lifts_to_cc_return_not_indirect() {
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    let arch = strider_target::SleighArch::aarch64();
    let regs = arch.probe_regs();
    let cc = strider_target::CallingConvention::aarch64_aapcs64()
        .build(&regs)
        .expect("build cc");
    // AArch64 `ret` = 0xD65F03C0, little-endian.
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![0xc0, 0x03, 0x5f, 0xd6], 0x1000);
    let mut lifter = super::Lifter::new(arch, reader).expect("lifter");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let outcome = lifter.build_ir(&cfg, cc).expect("build_ir");

    assert!(
        outcome.unresolved_branches.is_empty(),
        "`ret` must NOT defer an indirect branch; got {} unresolved",
        outcome.unresolved_branches.len(),
    );
    let function = &outcome.function;
    assert!(
        function.has_kind(|k| matches!(k, NodeKind::Return)),
        "`ret` must lift to a CC Return node"
    );
    assert!(
        !function.has_kind(|k| matches!(k, NodeKind::IndirectBranch)),
        "`ret` must NOT emit an IndirectBranch placeholder"
    );
}

/// AArch64 `svc` raises the `CallSupervisor` user-op, which has to carry a
/// `CallOtherAbi`: an unclassified user-op fails the whole function's lift.
#[test]
fn aarch64_svc_lifts_through_the_call_supervisor_abi() {
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    let arch = strider_target::SleighArch::aarch64();
    let regs = arch.probe_regs();
    let cc = strider_target::CallingConvention::aarch64_aapcs64()
        .build(&regs)
        .expect("build cc");
    // `svc #0` = 0xD4000001, `ret` = 0xD65F03C0, little-endian.
    let reader = rsleigh::mem_readers::BufMemReader::new(
        vec![0x01, 0x00, 0x00, 0xd4, 0xc0, 0x03, 0x5f, 0xd6],
        0x1000,
    );
    let mut lifter = super::Lifter::new(arch, reader).expect("lifter");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let outcome = lifter
        .build_ir(&cfg, cc)
        .expect("`svc` must lift, not fail the function");

    assert!(
        outcome
            .function
            .has_kind(|k| matches!(k, NodeKind::CallOther { .. })),
        "`svc` must lift to a CallOther node"
    );
}

/// A reused `Lifter` must decode each function in its own mode.  A Thumb
/// `BLX <imm>` that switches to ARM `globalset`s `TMode` for its target,
/// and that commit holds forward across `lift_one` calls.  `build_cfg` pins
/// each entry's mode (here Thumb, the `arm_thumb` default) at the entry
/// address, overriding the leaked commit; without it, lifting Thumb
/// function A (ending in such a `BLX`) then Thumb function B at an address
/// the commit reaches decodes B as ARM and walks off into unmapped memory.
#[test]
fn reused_lifter_pins_thumb_mode_between_functions() {
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    let arch = strider_target::SleighArch::arm_thumb();
    let regs = strider_target::SleighArch::arm_thumb().probe_regs();
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("cc");
    // Buffer at 0x1000; both entries are passed with the Thumb bit set
    // (0x1001 / 0x1011), which is what selects Thumb state.  The BLX
    // target is `((inst_start + 4) & !3) + (part2off_10 << 2)` = 0x1008,
    // and its `globalset(TMode = 0)` holds forward from there over B.
    //   0x1000: BLX 0x1008  (Thumb T2, switches to ARM at 0x1008)
    //   0x1004: bx lr       ends function A
    //   0x1006: nop x3      padding, then bx lr; nop at 0x100c
    //   0x1010: bx lr; nop   function B, valid Thumb
    let code = vec![
        0x00, 0xf0, 0x03, 0xe8, // 0x1000 BLX 0x1008
        0x70, 0x47, // 0x1004 bx lr
        0x00, 0xbf, 0x00, 0xbf, 0x00, 0xbf, // 0x1006 nop padding
        0x70, 0x47, 0x00, 0xbf, // 0x100c padding
        0x70, 0x47, 0x00, 0xbf, // 0x1010 function B
    ];
    let opts = strider_cfg::CfgOptions::default();
    let new_lifter = || {
        let reader = rsleigh::mem_readers::BufMemReader::new(code.clone(), 0x1000);
        super::Lifter::new(arch, reader).expect("lifter")
    };

    // Lift A (the polluter) first, then B on the same engine.
    let mut lifter = new_lifter();
    let cfg_a = lifter.build_cfg(0x1001u64.into(), &opts).expect("A cfg");
    lifter.build_ir(&cfg_a, cc.clone()).expect("A ir");
    let cfg_b = lifter.build_cfg(0x1011u64.into(), &opts).expect(
        "reused lifter must pin B's Thumb mode so its decode does not inherit A's ARM mode",
    );
    let reused_b = lifter.build_ir(&cfg_b, cc.clone()).expect("B ir");
    assert!(
        reused_b
            .function
            .has_kind(|k| matches!(k, NodeKind::Return)),
        "B (Thumb `bx lr`) lifted after A must decode as Thumb and emit a Return"
    );

    // Same B on a fresh lifter: the ground-truth decode.
    let mut fresh = new_lifter();
    let cfg_fresh = fresh
        .build_cfg(0x1011u64.into(), &opts)
        .expect("fresh B cfg");
    let fresh_b = fresh.build_ir(&cfg_fresh, cc).expect("fresh B ir");
    assert_eq!(
        reused_b.function.count_kind(|_| true),
        fresh_b.function.count_kind(|_| true),
        "B lifted after A must have the same node count as a fresh Thumb lift"
    );
}

/// Under the plain `arm` arch (ARM by default), a function whose entry
/// carries the Thumb bit (odd address) decodes as Thumb at `addr & !1`, and
/// the pinned mode holds forward past the entry: the second instruction (at
/// `addr + 2`) must also decode Thumb, not fall back to the ARM default.
#[test]
fn arm_arch_uses_the_thumb_bit_to_decode_a_thumb_function() {
    use strider_ir::node::NodeKind;
    use strider_ir_test_utils::IrWalkerEx;

    let arch = strider_target::SleighArch::arm();
    let regs = strider_target::SleighArch::arm().probe_regs();
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("cc");
    // Two Thumb instructions at 0x1000: `movs r0, #1` (0x1000) then `bx lr`
    // (0x1002). The `bx lr` is only reachable if 0x1002 also decodes Thumb;
    // as a 4-byte ARM insn it would run past the 4-byte buffer and fail.
    let code = vec![0x01, 0x20, 0x70, 0x47];
    let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
    let mut lifter = super::Lifter::new(arch, reader).expect("lifter");
    let opts = strider_cfg::CfgOptions::default();

    // Entry passed with the Thumb bit set (0x1001). Under the ARM default
    // this would try to decode the misaligned 0x1001 as ARM; the bit must
    // select Thumb and decode at 0x1000.
    let cfg = lifter
        .build_cfg(0x1001u64.into(), &opts)
        .expect("the Thumb bit must select Thumb mode and decode at addr & !1");
    let ir = lifter.build_ir(&cfg, cc).expect("ir");
    assert!(
        ir.function.has_kind(|k| matches!(k, NodeKind::Return)),
        "the `bx lr` at 0x1002 must lift to a Return, proving the pin held forward"
    );
}

/// Every register the arch DECLARES must have a `ValueType`, whether or
/// not the current sla references it: the tracked-varnode set is mapped
/// wholesale, so one unmappable width fails the whole function.  x86-64
/// declares `GDTR`/`IDTR` at 12 bytes and `LDTR`/`TR` at 14.
#[test]
fn every_declared_register_width_maps_to_a_value_type() {
    for arch in [
        strider_target::SleighArch::x86_64(),
        strider_target::SleighArch::x86(),
        strider_target::SleighArch::aarch64(),
        strider_target::SleighArch::arm(),
        strider_target::SleighArch::mipsle64(),
        strider_target::SleighArch::ppc64be(),
    ] {
        let preset = arch.preset();
        let regs = arch.probe_regs();
        let unmappable: Vec<(&str, u32)> = regs
            .iter()
            .filter(|r| strider_ir::ValueType::int_from_bytes(r.vn.size).is_err())
            .map(|r| (r.name, r.vn.size))
            .collect();
        assert!(
            unmappable.is_empty(),
            "{preset:?}: registers with no ValueType: {unmappable:?}"
        );
    }
}

/// `Phi` creation order comes from the placement set, so it must be sorted
/// by `InitialVnId` rather than left to hash layout: node-id assignment,
/// and every IR dump with it, otherwise moves with the hasher.
#[test]
fn placed_phis_are_ordered_by_initial_vn_id() {
    use strider_ir::node::NodeKind;
    use strider_ir::{IRViewer, IRWalker};

    let arch = strider_target::SleighArch::x86_64();
    let regs = strider_target::SleighArch::x86_64().probe_regs();
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("cc");
    // test edi,edi ; je +0x14 ; mov eax,1 ; mov ecx,2 ; mov edx,3 ;
    // mov esi,4 ; ret
    // Four registers live into one join region.
    let code = vec![
        0x85, 0xff, 0x74, 0x14, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xb9, 0x02, 0x00, 0x00, 0x00, 0xba,
        0x03, 0x00, 0x00, 0x00, 0xbe, 0x04, 0x00, 0x00, 0x00, 0xc3,
    ];
    let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
    let mut lifter = super::Lifter::new(arch, reader).expect("lifter");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let f = lifter.build_ir(&cfg, cc).expect("ir").function;

    // Ascending node id, so the group order below is creation order.
    let mut walk = f.walk();
    walk.by_ref().for_each(|_| {});
    let reachable = walk.into_visited();

    // Grouped by the region's phi token, so phis from
    // different regions do not interleave.
    let mut per_region: rustc_hash::FxHashMap<
        strider_ir::node::ValueId,
        Vec<strider_ir::node::InitialVnId>,
    > = rustc_hash::FxHashMap::default();
    for node in reachable
        .iter()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Phi))
    {
        let token = f.node_inputs(node)[NodeKind::PHI_TOKEN_SLOT];
        let [out] = f.node_outputs_exact::<1>(node).expect("phi output");
        let vn = f.get_vn_for_value(out).expect("phi carries a vn tag");
        let id = f.vn_id_of(&vn).expect("tagged vn is tracked");
        per_region.entry(token).or_default().push(id);
    }
    let widest = per_region
        .into_values()
        .max_by_key(Vec::len)
        .expect("at least one region with phis");
    assert!(widest.len() >= 2, "need a multi-phi join, got {widest:?}");
    assert!(
        widest.windows(2).all(|w| w[0] < w[1]),
        "ascending node ids must carry ascending InitialVnIds, got {widest:?}"
    );
}

/// The tracked set exists to serve `read_vn` / `write_vn`'s aliasing path,
/// which only REGISTER and UNIQUE take; CONST becomes a literal and RAM a
/// Load/Store against a constant address. Tracking either of the latter
/// mints an `InitialVar` for something that is not a variable, and widens
/// the per-region variable map `inherit_variables` clones.
#[test]
fn only_aliasable_space_varnodes_are_tracked() {
    let arch = strider_target::SleighArch::x86_64();
    // 1000: e8 00 00 00 00          call 0x1005      ; CONST inst_next
    // 1005: 48 8b 05 00 00 00 00    mov rax,[rip+0x0] ; RAM 0x100c
    // 100c: eb 00                   jmp 0x100e        ; RAM 0x100e
    // 100e: c3                      ret
    let code = vec![
        0xe8, 0x00, 0x00, 0x00, 0x00, 0x48, 0x8b, 0x05, 0x00, 0x00, 0x00, 0x00, 0xeb, 0x00, 0xc3,
    ];
    let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
    let mut lifter = super::Lifter::new(arch, reader).expect("lifter");
    let opts = strider_cfg::CfgOptions::default();
    let cfg = lifter.build_cfg(0x1000u64.into(), &opts).expect("cfg");

    let tracked: Vec<_> = super::aliasable_vns(&cfg).collect();
    assert!(!tracked.is_empty(), "the lift tracks something");
    let stray: Vec<_> = tracked
        .iter()
        .filter(|vn| !vn_container::is_aliasable_space(vn.addr_space))
        .map(|vn| (vn.addr_space.shortcut(), vn.addr_off, vn.size))
        .collect();
    assert!(
        stray.is_empty(),
        "only REGISTER/UNIQUE may be tracked; got {stray:?}"
    );
}

/// A `Cfg` carries no lifetime but holds each Load / Store's space id as a
/// raw `AddrSpace` pointer into the engine that decoded it, so lifting one
/// on a second engine reads that engine's memory, and lifting one whose
/// engine is gone reads freed memory.  Both are the same missing check.
#[test]
fn a_cfg_from_another_engine_is_rejected_before_its_space_ids_are_read() {
    let arch = strider_target::SleighArch::x86_64();
    let regs = strider_target::SleighArch::x86_64().probe_regs();
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    // `mov rax, [rbx]` then `ret`: the load is what carries a space id.
    let code = vec![0x48u8, 0x8b, 0x03, 0xc3];
    let opts = strider_cfg::CfgOptions::default();
    let new_lifter = || {
        let reader = rsleigh::mem_readers::BufMemReader::new(code.clone(), 0x1000);
        super::Lifter::new(arch, reader).expect("lifter")
    };

    let mut owner = new_lifter();
    let cfg = owner.build_cfg(0x1000u64.into(), &opts).expect("cfg");
    assert!(
        cfg.regions()
            .flat_map(|region| region.insns.iter())
            .any(|wrapped| wrapped.insn.opcode == rsleigh::Opcode::Load),
        "the fixture must decode to a LOAD, or the check under test never runs"
    );

    let foreign = new_lifter();
    let Err(err) = foreign.build_ir(&cfg, cc.clone()) else {
        panic!("a CFG from another engine must not lift")
    };
    let msg = err.to_string();
    assert!(
        msg.contains("did not declare"),
        "the error must name the foreign space id; got: {msg}"
    );

    owner
        .build_ir(&cfg, cc)
        .expect("the same CFG must still lift on the engine that built it");
}

/// Every preset's [`strider_target::DecodeTables`] build, whose checks
/// panic rather than err: a non-empty user-op table, so no CALLOTHER is
/// lifted against an empty one, and `VnSpace::RAM` pinned to the space
/// actually named `ram`.
#[test]
fn every_preset_has_a_user_op_table_and_an_ram_named_ram() {
    for preset in strider_target::ArchPreset::ALL {
        let arch = preset.arch();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![0u8; 0x10], 0x1000);
        let lifter = super::Lifter::new(arch, reader)
            .unwrap_or_else(|e| panic!("{preset:?}: Lifter::new must succeed; got {e}"));
        assert!(
            !lifter.user_op_names().is_empty(),
            "{preset:?}: an empty user-op table makes every CALLOTHER fail"
        );
    }
}

/// `test edi,edi ; je 0x100a ; mov eax,1 ; ret ; xor eax,eax ; ret`. The
/// builder creates the two arms in queue order, and petgraph lists a
/// region's out-edges newest first, so the depth-first discovery reaches
/// the arm with the higher `RegionId` first. The SSA walk, whose order
/// the body nodes' NodeIds follow, takes siblings by `RegionId`.
#[test]
fn the_ssa_walk_takes_sibling_regions_by_region_id() {
    let arch = strider_target::SleighArch::x86_64();
    let code = vec![
        0x85, 0xff, 0x74, 0x06, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0x31, 0xc0, 0xc3,
    ];
    let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
    let mut lifter = super::Lifter::new(arch, reader).expect("lifter");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let entry = cfg.entry();
    let mut arms: Vec<strider_cfg::RegionId> = cfg.region_ids().filter(|&r| r != entry).collect();
    arms.sort_unstable();
    assert_eq!(arms.len(), 2, "premise: an entry and two arms");
    let graph = cfg.region_graph();
    let doms = graph_algorithms::dominance::dominators(entry, |r| graph.neighbors(r));
    assert_eq!(
        doms.vertices(),
        [entry, arms[1], arms[0]],
        "premise: discovery reaches the later arm first"
    );
    assert_eq!(doms.tree_preorder(), [entry, arms[0], arms[1]]);
}
