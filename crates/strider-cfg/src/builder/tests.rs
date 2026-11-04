use petgraph::visit::{EdgeRef, IntoEdgeReferences};

use crate::test_support::*;
use crate::types::{PcodeInsnAddr, Region, RegionTerminator};

/// x86-64 `movabs rax, 0`: ten bytes, so `base + 5` is interior to it and
/// not an instruction boundary.
const MOVABS_RAX_0: [u8; 10] = [0x48, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0];

/// The pcode index of the `BranchIndirect` in the machine instruction at
/// `at`.
fn branch_indirect_index(bytes: &[u8], base: u64, at: u64) -> u64 {
    let mut sleigh = make_sleigh_over(bytes.to_vec(), base);
    let lift = sleigh.lift_one(at).expect("lift_one");
    lift.insns
        .iter()
        .position(|i| i.opcode == rsleigh::Opcode::BranchIndirect)
        .expect("no BranchIndirect at that address") as u64
}

fn build_cfg(
    bytes: Vec<u8>,
    base: u64,
    options: &crate::CfgOptions<'_>,
) -> crate::Result<crate::Cfg> {
    crate::test_support::make_engine_over(bytes, base).build_cfg(base.into(), options)
}

/// Every `Switch` arm must resolve to a successor region starting exactly at
/// it; the lifter errors out on a target with no arm.
fn assert_every_switch_target_has_an_arm(cfg: &crate::Cfg) {
    for rid in cfg.region_ids() {
        let RegionTerminator::Switch { targets, .. } = cfg
            .region_graph()
            .node_weight(rid)
            .unwrap()
            .terminator
            .clone()
        else {
            continue;
        };
        let arms = cfg.switch_arm_regions(rid);
        for t in targets {
            assert!(
                arms.contains_key(&PcodeInsnAddr::at_machine_start(t.addr)),
                "switch target {:#x} has no successor region",
                t.addr,
            );
        }
    }
}

/// A `je` into the middle of a ten-byte `movabs` is interior to the region
/// but off every instruction boundary. It reaches `split_region` with no
/// guard in front of it, so it must degrade rather than fail the build.
#[test]
fn direct_cond_branch_into_an_instruction_interior_still_builds() {
    let base = 0x1000u64;
    let mut bytes = MOVABS_RAX_0.to_vec();
    bytes.extend_from_slice(&[0x74, 0xf9]); // 0x100a: je 0x1005
    bytes.push(0xc3); // 0x100c: ret

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default())
        .expect("a branch into an instruction interior must not fail the build");
    // The conditional survives with both arms, so the lifter can still
    // recover its polarity.
    let cond = cfg
        .region_ids()
        .find(|&r| {
            matches!(
                cfg.region_graph().node_weight(r).unwrap().terminator,
                RegionTerminator::CondBranch { .. }
            )
        })
        .expect("cond-branch region");
    let succ = cfg.region_if(cond).expect("region_if");
    assert!(succ.if_true_region.is_some() && succ.if_false_region.is_some());
}

/// The start of the one region whose terminator is a `TailCall`.
fn tail_call_stub_start(cfg: &crate::Cfg) -> PcodeInsnAddr {
    let mut stubs = cfg.region_ids().filter(|&r| {
        matches!(
            cfg.region_graph().node_weight(r).unwrap().terminator,
            RegionTerminator::TailCall { .. }
        )
    });
    let stub = stubs.next().expect("no tail-call stub");
    assert!(stubs.next().is_none(), "more than one tail-call stub");
    cfg.region_graph().node_weight(stub).unwrap().start_addr
}

/// `jmp rel32` far past the end of the mapped buffer: a firmware window a
/// branch leaves, an unrelocated `jmp` in an `ET_REL` object.
#[test]
fn a_direct_branch_to_an_unmapped_address_does_not_fail_the_function() {
    let base = 0x1000u64;
    let target = 0x4000_1005u64;
    let mut bytes = vec![0xe9, 0x00, 0x00, 0x00, 0x40]; // 0x1000: jmp 0x40001005
    bytes.extend_from_slice(&[0xc3; 8]);

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default())
        .expect("an unmapped direct target must not fail the whole function");
    assert_eq!(
        cfg.incomplete()
            .of(crate::Reason::UnmappedTarget)
            .collect::<Vec<_>>(),
        [PcodeInsnAddr::at_machine_start(target)],
    );
    assert_eq!(
        tail_call_stub_start(&cfg),
        PcodeInsnAddr::at_machine_start(target),
    );
}

/// The fall-through arm decodes fine, so failing the build would throw away
/// a whole reachable half of the function over the other arm.
#[test]
fn an_unmapped_conditional_arm_keeps_the_fall_through() {
    let base = 0x1000u64;
    let target = 0x4000_1006u64;
    let mut bytes = vec![0x0f, 0x85, 0x00, 0x00, 0x00, 0x40]; // jne 0x40001006
    bytes.extend_from_slice(&[0xc3; 8]);

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default())
        .expect("an unmapped arm must not fail the whole function");
    assert_eq!(
        cfg.incomplete()
            .of(crate::Reason::UnmappedTarget)
            .collect::<Vec<_>>(),
        [PcodeInsnAddr::at_machine_start(target)],
    );
    let cond = cfg
        .region_ids()
        .find(|&r| {
            matches!(
                cfg.region_graph().node_weight(r).unwrap().terminator,
                RegionTerminator::CondBranch { .. }
            )
        })
        .expect("cond-branch region");
    let succ = cfg.region_if(cond).expect("region_if");
    assert!(
        succ.if_true_region.is_some() && succ.if_false_region.is_some(),
        "both arms must survive, the taken one through the stub",
    );
}

/// A function whose first byte is unmapped has nothing to analyse, so there
/// is no partial answer to report.
#[test]
fn an_unmapped_entry_is_still_an_error() {
    let err = crate::test_support::make_engine_over(vec![0xc3; 8], 0x1000)
        .build_cfg(0x9000u64.into(), &crate::CfgOptions::default())
        .expect_err("an unmapped entry has no function to report on");
    assert!(
        err.to_string().contains("0x9000"),
        "the error must name the entry; got {err}",
    );
}

/// A branch target interior to a region but off every instruction boundary
/// is seated on the region that OWNS those bytes.  Splitting that region at
/// an EARLIER boundary hands the bytes to the second half, so the seated
/// edge has to move with them; retargeting it to the first half the way
/// every other incoming edge moves leaves the branch with no successor
/// holding its target, and `region_if` reports no taken side.
///
/// Byte layout, chosen so the seat is wired BEFORE the split under the LIFO
/// work queue: the entry's taken arm (0x1011, explored last) branches back
/// to the boundary 0x1002, while its fall-through arm seats 0x1005,
/// interior to the `movabs` at 0x1002.
#[test]
fn a_split_before_a_seated_interior_target_keeps_the_edge_on_the_owning_half() {
    let base = 0x1000u64;
    let mut bytes = vec![0x31, 0xc0]; // 0x1000: xor eax, eax
    bytes.extend_from_slice(&MOVABS_RAX_0); // 0x1002..0x100c
    bytes.extend_from_slice(&[0x74, 0x03]); // 0x100c: je 0x1011
    bytes.extend_from_slice(&[0x74, 0xf5]); // 0x100e: je 0x1005
    bytes.push(0xc3); // 0x1010: ret
    bytes.extend_from_slice(&[0xeb, 0xef]); // 0x1011: jmp 0x1002

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default()).expect("build");

    let seat_parent = cfg
        .region_ids()
        .find(|&r| {
            cfg.region_graph().node_weight(r).unwrap().terminator
                == RegionTerminator::CondBranch {
                    true_target: addr(0x1005, 0),
                }
        })
        .expect("region conditionally branching to 0x1005");

    let successors: std::collections::BTreeSet<PcodeInsnAddr> = cfg
        .region_graph()
        .neighbors(seat_parent)
        .map(|s| cfg.region_graph().node_weight(s).unwrap().start_addr)
        .collect();
    assert_eq!(
        successors,
        std::collections::BTreeSet::from([addr(0x1002, 0), addr(0x1010, 0)]),
        "taken arm must be the half that owns 0x1005, fall-through the ret at 0x1010"
    );

    let succ = cfg.region_if(seat_parent).expect("region_if");
    let taken = succ.if_true_region.expect("taken side");
    assert!(
        cfg.region_graph()
            .node_weight(taken)
            .unwrap()
            .contains_addr(addr(0x1005, 0))
    );
    assert_eq!(
        cfg.region_graph()
            .node_weight(succ.if_false_region.expect("fall-through side"))
            .unwrap()
            .start_addr,
        addr(0x1010, 0)
    );
}

/// The same interior target, but reached when the multi-byte instruction is
/// its region's LAST one: the `nop` seals the region right after `movabs`,
/// so the span has to cover the `movabs` bytes or the guard never sees the
/// target and a fresh region decodes mid-immediate.
#[test]
fn direct_cond_branch_into_the_last_instructions_interior_still_builds() {
    let base = 0x1000u64;
    let mut bytes = MOVABS_RAX_0.to_vec();
    bytes.push(0x90); // 0x100a: nop, zero pcode ops: seals the movabs region
    bytes.extend_from_slice(&[0x74, 0xf8]); // 0x100b: je 0x1005
    bytes.push(0xc3); // 0x100d: ret

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default())
        .expect("a branch into the last instruction's interior must not fail the build");
    let cond = cfg
        .region_ids()
        .find(|&r| {
            matches!(
                cfg.region_graph().node_weight(r).unwrap().terminator,
                RegionTerminator::CondBranch { .. }
            )
        })
        .expect("cond-branch region");
    let succ = cfg.region_if(cond).expect("region_if");
    assert!(succ.if_true_region.is_some() && succ.if_false_region.is_some());
}

/// A resolved jump-table arm landing inside the LAST instruction of an
/// already-decoded region. The bytes there decode cleanly, so nothing errors
/// out; the arm must still be dropped rather than seated on a second region
/// overlapping the `movabs`.
#[test]
fn switch_target_interior_to_a_regions_last_instruction_is_dropped() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0]; // 0x1000: jmp rax
    let mut movabs = MOVABS_RAX_0; // 0x1002..0x100c: movabs rax, imm64
    movabs[3] = 0xc3; // the immediate byte at 0x1005 decodes as `ret`
    bytes.extend_from_slice(&movabs);
    bytes.push(0x90); // 0x100c: nop; seals the movabs region
    bytes.push(0xc3); // 0x100d: ret

    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1005.into(), 0x1002.into()]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts)
        .expect("a table target off an instruction boundary must not fail the build");
    assert_every_switch_target_has_an_arm(&cfg);
    assert!(
        !cfg.regions().any(|r| r.start_addr.machine() == 0x1005),
        "a region was decoded inside the movabs immediate: {:?}",
        cfg.regions()
            .map(|r| (r.start_addr, r.terminator.clone()))
            .collect::<Vec<_>>(),
    );
    let targets = cfg
        .regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .expect("switch region");
    assert_eq!(
        targets.iter().map(|t| t.addr).collect::<Vec<_>>(),
        vec![0x1002],
        "the off-boundary arm must be dropped from the table",
    );
}

/// A resolved jump-table target interior to the switch's OWN region: the
/// seat-time guard runs before `finish_current_region`, so no region owns
/// the address yet and the target slips through.
#[test]
fn switch_target_interior_to_its_own_region_still_builds() {
    let base = 0x1000u64;
    let mut bytes = MOVABS_RAX_0.to_vec();
    bytes.extend_from_slice(&[0xff, 0xe0]); // 0x100a: jmp rax
    bytes.push(0xc3); // 0x100c: ret

    let branch = PcodeInsnAddr {
        machine_addr: 0x100a.into(),
        insn_index: branch_indirect_index(&bytes, base, 0x100a),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1005.into(), 0x100c.into()]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts)
        .expect("a table target off an instruction boundary must not fail the build");
    assert_every_switch_target_has_an_arm(&cfg);
}

/// Same defect, reached the other way: the target is on no boundary of a
/// region decoded AFTER the switch was seated, so exploration order alone
/// decides whether the site defers or explodes.
#[test]
fn switch_target_interior_to_a_later_decoded_region_still_builds() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0]; // 0x1000: jmp rax
    bytes.extend_from_slice(&MOVABS_RAX_0); // 0x1002..0x100b
    bytes.push(0xc3); // 0x100c: ret

    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    // Supplied high-first: `table` sorts, so 0x1002 is still explored
    // first and its region then covers 0x1005.
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1005.into(), 0x1002.into()]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts)
        .expect("a table target off an instruction boundary must not fail the build");
    assert_every_switch_target_has_an_arm(&cfg);
}

/// The same address twice in one table, once per ISA mode: x86 has no
/// mode var, so both arms decode alike and both reach the interior-target
/// seat.
#[test]
fn one_address_in_both_modes_interior_to_a_later_decoded_region_still_builds() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0]; // 0x1000: jmp rax
    bytes.extend_from_slice(&MOVABS_RAX_0); // 0x1002..0x100b
    bytes.push(0x90); // 0x100c: nop
    bytes.push(0xc3); // 0x100d: ret

    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table([
            crate::ResolvedTarget::new(0x1005, Some(false)),
            crate::ResolvedTarget::new(0x1005, Some(true)),
            0x1002.into(),
        ]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts)
        .expect("a duplicated off-boundary table target must not fail the build");
    assert_every_switch_target_has_an_arm(&cfg);
    let targets = cfg
        .regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .expect("switch region");
    assert_eq!(
        targets.iter().map(|t| t.addr).collect::<Vec<_>>(),
        vec![0x1002],
        "both modes of the off-boundary arm must be dropped from the table",
    );
}

/// A resolver's own answer beats the caller's at the site, whichever key
/// the caller used, and `Withdrawn` defers the site rather than letting the
/// caller's seed through.
#[test]
fn a_derived_answer_outranks_the_seed_and_an_empty_one_defers() {
    let base = 0x1000u64;
    let mut bytes = vec![0xc3u8; 0x30]; // ret everywhere
    bytes[..2].copy_from_slice(&[0xff, 0x20]); // jmp [rax]
    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    assert_ne!(
        branch.insn_index, 0,
        "precondition: the exact and machine-start keys must differ"
    );
    let arms = |cfg: &crate::Cfg| {
        cfg.regions().find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, .. } => {
                Some(targets.iter().map(|t| t.addr).collect::<Vec<_>>())
            }
            _ => None,
        })
    };
    let derived: rustc_hash::FxHashMap<_, _> =
        std::iter::once((branch, crate::ResolvedTargets::table(vec![0x1020.into()]))).collect();
    let empty: rustc_hash::FxHashMap<_, _> =
        std::iter::once((branch, crate::ResolvedTargets::Withdrawn)).collect();
    for seed_key in [branch, PcodeInsnAddr::at_machine_start(base)] {
        let known: rustc_hash::FxHashMap<_, _> =
            std::iter::once((seed_key, crate::ResolvedTargets::table(vec![0x1010.into()])))
                .collect();
        let cfg = build_cfg(
            bytes.clone(),
            base,
            &crate::CfgOptions {
                known_targets: &known,
                derived_targets: &derived,
                ..crate::CfgOptions::default()
            },
        )
        .expect("build");
        assert_eq!(arms(&cfg), Some(vec![0x1020]), "{seed_key:?}");
        let cfg = build_cfg(
            bytes.clone(),
            base,
            &crate::CfgOptions {
                known_targets: &known,
                derived_targets: &empty,
                ..crate::CfgOptions::default()
            },
        )
        .expect("build");
        assert_eq!(
            arms(&cfg),
            None,
            "{seed_key:?}: an empty answer seats nothing"
        );
        assert!(
            cfg.regions().any(|r| matches!(
                &r.terminator,
                RegionTerminator::UnresolvedIndirectBranch { addr, .. } if *addr == branch
            )),
            "{seed_key:?}: an empty answer defers the site",
        );
    }
}

/// Overlapping code: sequential decoding steps OVER a region start that is
/// interior to an instruction it decodes, so two regions own those bytes
/// with two different instruction streams.
///
/// ```text
/// 1000  je 0x1010
/// 1002  jmp 0x1014      ; decodes 0x1014 as a region of its own
/// 1010  movabs rax, i64 ; ten bytes, spanning 0x1014
/// 101a  ret
/// ```
///
/// The fall-through seal is an exact-key lookup, so nothing on the decode
/// path lands on 0x1014 and no other reason names it.
#[test]
fn a_region_start_stepped_over_by_a_later_decode_is_reported() {
    let base = 0x1000u64;
    let mut bytes = vec![0x74, 0x0e]; // 0x1000: je 0x1010
    bytes.extend_from_slice(&[0xeb, 0x10]); // 0x1002: jmp 0x1014
    bytes.extend_from_slice(&[0x90; 12]); // 0x1004..0x1010: never decoded
    // 0x1010: movabs rax, imm64; the immediate byte at 0x1014 is `ret`.
    bytes.extend_from_slice(&[0x48, 0xb8, 0x00, 0x00, 0xc3, 0x00, 0x00, 0x00, 0x00, 0x00]);
    bytes.push(0xc3); // 0x101a: ret

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default()).expect("build");

    assert_eq!(
        cfg.regions()
            .filter(|r| r.contains_addr(addr(0x1014, 0)))
            .count(),
        2,
        "the overlap this pins must actually happen",
    );
    assert_eq!(
        cfg.incomplete()
            .of(crate::Reason::InteriorTarget)
            .collect::<Vec<_>>(),
        &[addr(0x1014, 0)]
    );
}

/// ```text
/// 1000  mov eax, 0xc3000000 ; the immediate byte at 0x1004 is `ret`
/// 1005  je 0x1004
/// 1007  je 0x1004
/// 1009  ret
/// ```
///
/// Both branches seat on the `mov` region, which owns 0x1004 but has no
/// instruction there.
#[test]
fn two_branches_into_one_interior_address_report_it_once() {
    let base = 0x1000u64;
    let bytes = vec![
        0xb8, 0x00, 0x00, 0x00, 0xc3, // 0x1000: mov eax, 0xc3000000
        0x74, 0xfd, // 0x1005: je 0x1004
        0x74, 0xfb, // 0x1007: je 0x1004
        0xc3, // 0x1009: ret
    ];

    let cfg = build_cfg(bytes, base, &crate::CfgOptions::default()).expect("build");

    assert_eq!(
        cfg.incomplete()
            .of(crate::Reason::InteriorTarget)
            .collect::<Vec<_>>(),
        &[addr(0x1004, 0)]
    );
}

/// A seeded arm that will not decode is reported against the site that
/// named it, not by address alone.
#[test]
fn an_undecodable_seeded_target_carries_its_site() {
    let base = 0x1000u64;
    let bytes = vec![0xff, 0xe0, 0xc3]; // jmp rax; ret

    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1002.into(), 0x9000.into()]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts).expect("build");
    assert_eq!(
        cfg.undecodable_seeded_targets(),
        &[crate::UndecodableTarget {
            site: branch,
            target: addr(0x9000, 0),
        }],
    );
    assert!(
        cfg.incomplete()
            .iter()
            .any(|e| e == (addr(0x9000, 0), crate::Reason::UnmappedTarget)),
        "the dropped arm is named for what it is: {:?}",
        cfg.incomplete()
    );
    assert_every_switch_target_has_an_arm(&cfg);
}

/// An arm whose code falls through past the function's size is dropped,
/// and named where it ran off.
///
/// ```text
/// 1000  jmp rax
/// 1002  ret
/// 1003  nop        ; falls through past the 4-byte bound
/// ```
#[test]
fn a_seeded_arm_failing_past_the_bound_is_named() {
    let base = 0x1000u64;
    let bytes = vec![0xff, 0xe0, 0xc3, 0x90, 0xc3];

    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1002.into(), 0x1003.into()]),
    );
    let opts = crate::CfgOptions {
        bound: crate::FunctionBound::size(4),
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts).expect("build");
    assert_eq!(
        cfg.undecodable_seeded_targets(),
        &[crate::UndecodableTarget {
            site: branch,
            target: addr(0x1003, 0),
        }],
    );
    assert_eq!(
        cfg.incomplete()
            .iter()
            .filter(|(_, reason)| !reason.is_site())
            .collect::<Vec<_>>(),
        [(addr(0x1004, 0), crate::Reason::PastFunctionEnd)],
    );
    assert_every_switch_target_has_an_arm(&cfg);
}

/// An out-of-range arm (a `switch` case that tail-calls) is evidence the
/// bound is wrong, so the whole site defers rather than seating a `Switch`
/// on the arms that are in range.
#[test]
fn an_out_of_range_switch_arm_defers_the_whole_table() {
    let base = 0x1000u64;
    let bytes = vec![0xff, 0xe0, 0xc3, 0xc3]; // jmp rax; ret; ret

    let branch = PcodeInsnAddr {
        machine_addr: base.into(),
        insn_index: branch_indirect_index(&bytes, base, base),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1002.into(), 0x1003.into(), 0x9000.into()]),
    );
    let opts = crate::CfgOptions {
        bound: crate::FunctionBound::size(0x10),
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts).expect("build");
    assert_every_switch_target_has_an_arm(&cfg);
    assert!(
        cfg.regions()
            .all(|r| !matches!(r.terminator, RegionTerminator::Switch { .. })),
        "an out-of-range arm is evidence the bound is wrong, so the site defers",
    );
}

/// An arm already interior to a decoded region when the site is sealed
/// defers the site, and is reported the way `seat_non_boundary_target`
/// reports the one discovered later.
#[test]
fn a_seat_time_interior_switch_arm_is_reported_and_defers() {
    let base = 0x1000u64;
    let mut bytes = MOVABS_RAX_0.to_vec(); // 0x1000..0x100a
    bytes.push(0x90); // 0x100a: nop, seals the movabs region
    bytes.extend_from_slice(&[0xff, 0xe0]); // 0x100b: jmp rax
    bytes.push(0xc3); // 0x100d: ret

    let branch = PcodeInsnAddr {
        machine_addr: 0x100b.into(),
        insn_index: branch_indirect_index(&bytes, base, 0x100b),
    };
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        branch,
        crate::ResolvedTargets::table(vec![0x1005.into(), 0x100d.into()]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };

    let cfg = build_cfg(bytes, base, &opts).expect("build");
    assert_every_switch_target_has_an_arm(&cfg);
    assert_eq!(
        cfg.incomplete()
            .of(crate::Reason::InteriorTarget)
            .collect::<Vec<_>>(),
        &[addr(0x1005, 0)],
        "an arm off every boundary must be reported, not only deferred",
    );
}

/// The assert's real purpose: an interior target naming NO arm of the
/// `Switch` that routed it is a construction bug, and stays one now that a
/// repeated arm is tolerated. Debug-only, since the assert is compiled out
/// of a release test run.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "names no arm of the Switch")]
fn seating_an_interior_target_naming_no_arm_trips_the_invariant() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let owner = b.add_region(make_region(&[(0x2000, 0)])).unwrap();
    let mut dispatch = make_region(&[(0x1000, 0)]);
    dispatch.terminator = RegionTerminator::Switch {
        target_vn: rsleigh::Vn {
            size: 8,
            addr_off: 0,
            addr_space: rsleigh::VnSpace::REGISTER,
        },
        targets: vec![0x2000.into()],
        addr: addr(0x1000, 0),
    };
    let parent = b.add_region(dispatch).unwrap();
    let _ = b.seat_non_boundary_target(parent, owner, addr(0x3000, 0));
}

/// The flowing bit a site records is the mode its own region decoded in,
/// and is NOT recoverable once the build is over.
///
/// ARM at 0x1000:
///
/// ```text
/// 1000  b 0x1010
/// 1008  (Thumb) b .        ; the seeded interworking arm
/// 1010  bx r0              ; the dispatch, seated with two arms
/// 1020  bx lr              ; the mode-less arm
/// ```
///
/// The dispatch's region never drifts, so nothing splits the context at
/// 0x1010; the Thumb arm at 0x1008 is explored afterwards and paints its
/// own mode forward across the dispatch. A read after the build then calls
/// the ARM dispatch Thumb.
#[test]
fn a_later_thumb_region_repaints_the_dispatch_a_site_already_sampled() {
    let base = 0x1000u64;
    let arch = strider_target::SleighArch::arm();
    let mut bytes = vec![0u8; 0x30];
    let mut put = |at: u64, word: u32| {
        let off = (at - base) as usize;
        bytes[off..off + 4].copy_from_slice(&word.to_le_bytes());
    };
    for i in 0..0xc {
        put(base + i * 4, 0xe12f_ff1e); // bx lr
    }
    put(0x1000, 0xea00_0002); // b 0x1010
    put(0x1008, 0x0000_e7fe); // (Thumb) b .
    put(0x1010, 0xe12f_ff10); // bx r0

    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, base);
    let mut engine = crate::DecodeEngine::new(arch, reader).expect("decode engine");
    let mut known_targets = rustc_hash::FxHashMap::default();
    known_targets.insert(
        PcodeInsnAddr::at_machine_start(0x1010),
        crate::ResolvedTargets::table(vec![
            crate::ResolvedTarget::new(0x1008, Some(true)),
            crate::ResolvedTarget::new(0x1020, None),
        ]),
    );
    let opts = crate::CfgOptions {
        known_targets: &known_targets,
        ..crate::CfgOptions::default()
    };
    let cfg = engine.build_cfg(base.into(), &opts).expect("build");

    let dispatch = cfg
        .regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { addr, .. } => Some(*addr),
            _ => None,
        })
        .expect("switch region");
    assert_eq!(
        cfg.flowing_isa_bit_at_site(dispatch),
        Some(false),
        "the dispatch's own region decoded as ARM",
    );
    assert!(
        engine
            .sleigh()
            .get_context_at(dispatch.machine(), arch.isa_mode_var().expect("arm"))
            .is_ok_and(|mode| mode != 0),
        "premise: a read after the build must see the Thumb arm's paint, or \
         the seal-time sample is proving nothing",
    );
    let insn_len = |start: u64| {
        cfg.regions()
            .find(|r| r.start_addr.machine() == start)
            .and_then(|r| r.insns.first())
            .map(|i| i.len)
    };
    assert_eq!(insn_len(0x1008), Some(2), "the interworking arm is Thumb");
    assert_eq!(
        insn_len(0x1020),
        Some(4),
        "a mode-less arm decodes in the bit the seal sampled, not the one \
         the Thumb arm painted over it",
    );
}

/// A direct edge reads nothing of the engine AT ITS TARGET: the ISA mode
/// is the parent region's and the flowing commits are read at the parent's
/// own last address, where a snapshot of the target would both cost a read
/// per flow var per edge and answer with whatever a previous `build_cfg`
/// on this engine committed there.
#[test]
fn enqueue_reads_no_context_off_a_commit_point() {
    use crate::builder::flow::CONTEXT_READS;
    let arch = strider_target::SleighArch::arm();
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![0u8; 0x100], 0x1000);
    let mut engine = crate::DecodeEngine::new(arch, reader).expect("decode engine");

    CONTEXT_READS.with(|n| n.set(0));
    let _ = engine.flow_vars().snapshot(&engine.sleigh, 0x1000);
    let per_snapshot = CONTEXT_READS.with(std::cell::Cell::get);
    assert!(per_snapshot > 1, "premise: the ARM sla flows several vars");

    let opts = crate::CfgOptions::default();
    let mut b = engine.builder(0x1000u64.into(), &opts).expect("builder");
    CONTEXT_READS.with(|n| n.set(0));
    b.enqueue(None, addr(0x1000, 0), Some(1), &[]);
    assert_eq!(CONTEXT_READS.with(std::cell::Cell::get), 0);
    let var = arch.isa_mode_var().expect("ARM has an ISA-mode var");
    assert_eq!(
        b.engine
            .flow_vars()
            .value_of(&b.work_queue.last().expect("queued").carried, var),
        Some(1),
        "the child carries the parent's mode",
    );
}

/// `enqueue` reads the engine only at an address THIS build committed.
/// Anywhere else a snapshot answers with whatever a previous `build_cfg`
/// on the same engine left there, and the entry pin cannot undo it: the
/// forward paint stops at the next commit of the same var.
#[test]
fn enqueue_ignores_a_context_commit_this_build_did_not_make() {
    let arch = strider_target::SleighArch::arm();
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![0u8; 0x100], 0x1000);
    let mut engine = crate::DecodeEngine::new(arch, reader).expect("decode engine");

    // Pure IT-block state: `enqueue` takes only the ISA-mode var from the
    // parent, so this one comes from the baseline either way.
    let victim = "cond_mask";
    assert!(
        arch.decode_tables().flow_vars.iter().any(|n| n == victim),
        "the ARM sla flows {victim}"
    );
    assert_ne!(arch.isa_mode_var(), Some(victim));

    let entry = engine.flow_vars().snapshot(&engine.sleigh, 0x1000);
    let clean = engine.flow_vars().value_of(&entry, victim).expect("read");

    // A previous function's `globalset`, still held forward at the target.
    let stale = clean ^ 1;
    engine
        .sleigh
        .set_context_at(0x1040, victim, stale)
        .expect("commit a stale context");

    // ARM re-derives committed context, so a snapshot is trusted only
    // where this build committed.
    let opts = crate::CfgOptions::default();
    let mut b = engine.builder(0x1000u64.into(), &opts).expect("builder");
    b.enqueue(None, addr(0x1040, 0), None, &[]);
    assert_eq!(
        b.engine
            .flow_vars()
            .value_of(&b.work_queue.last().expect("queued").carried, victim),
        Some(clean),
        "a commit this build never made must not be carried",
    );

    // The same address once this build has decoded into it: its own commit
    // is what the edges inside a split IT block need.
    b.committed_next.insert(0x1040);
    b.enqueue(None, addr(0x1040, 0), None, &[]);
    assert_eq!(
        b.engine
            .flow_vars()
            .value_of(&b.work_queue.last().expect("queued").carried, victim),
        Some(stale),
        "this build's own commit IS carried",
    );
}

/// On an arch naming no `flow_commit_vars`, `rederive_flow` is a no-op, so
/// `committed_next` vouches for nothing and a snapshot would answer with
/// whatever ANY decode left -- including `is_benign_prefix_reentry`'s
/// throwaway lift of a mid-instruction address, whose commits are nobody's
/// `inst_next` to re-derive. The baseline is the only honest answer there.
#[test]
fn enqueue_reads_the_engine_only_where_this_build_re_derives() {
    let arch = strider_target::SleighArch::x86_64();
    assert!(
        arch.flow_commit_vars().is_empty(),
        "x86-64 commits nothing to re-derive",
    );
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![0u8; 0x100], 0x1000);
    let mut engine = crate::DecodeEngine::new(arch, reader).expect("decode engine");
    let victim = engine
        .sleigh
        .flow_context_vars()
        .expect("flow vars")
        .into_iter()
        .next()
        .expect("x86-64 declares flowing vars");

    let entry = engine.flow_vars().snapshot(&engine.sleigh, 0x1000);
    let clean = engine.flow_vars().value_of(&entry, &victim).expect("read");
    engine
        .sleigh
        .set_context_at(0x1040, &victim, clean ^ 1)
        .expect("commit out of band");

    let opts = crate::CfgOptions::default();
    let mut b = engine.builder(0x1000u64.into(), &opts).expect("builder");
    // Vouched for, and still not read: nothing re-derived it.
    b.committed_next.insert(0x1040);
    b.enqueue(None, addr(0x1040, 0), None, &[]);
    assert_eq!(
        b.engine
            .flow_vars()
            .value_of(&b.work_queue.last().expect("queued").carried, &victim),
        Some(clean),
        "a snapshot was trusted on an arch that re-derives nothing",
    );
}

/// An empty region is a zero-pcode-op instruction's (`Unconditional`) or a
/// stub (`TailCall`); any other terminator on one is a construction bug.
#[test]
fn add_region_admits_an_empty_region_by_terminator() {
    for (terminator, ok) in [
        (RegionTerminator::Unconditional, true),
        (
            RegionTerminator::TailCall {
                target: 0x1000.into(),
            },
            true,
        ),
        (RegionTerminator::Return, false),
    ] {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let empty = Region {
            start_addr: addr(0x1000, 0),
            insns: Vec::new(),
            empty_span_len: 0,
            isa_mode: None,
            terminator: terminator.clone(),
        };
        match b.add_region(empty) {
            Ok(_) => assert!(ok, "{terminator:?}"),
            Err(err) => {
                assert!(!ok, "{terminator:?}: {err}");
                assert!(err.to_string().contains("has no instructions"), "{err}");
            }
        }
    }
}

#[test]
fn add_region_indexes_each_region_by_its_start() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let id1 = b.add_region(make_region(&[(0x1000, 0)])).unwrap();
    let id2 = b.add_region(make_region(&[(0x1010, 0)])).unwrap();

    assert_ne!(id1, id2);
    assert_eq!(b.region_graph.node_count(), 2);
    assert_eq!(b.index.get(addr(0x1000, 0)), Some(id1));
    assert_eq!(b.index.get(addr(0x1010, 0)), Some(id2));
}

/// A `Switch` whose last over-approximated arm lands off every instruction
/// boundary demotes to `UnresolvedIndirectBranch` rather than surviving as
/// a table with zero arms, and carries the dispatch address the terminator
/// recorded so a later round re-seats the same site.
///
/// The recorded dispatch differs from the region's last instruction, so the
/// assertion pins which of the two the demotion uses.
#[test]
fn emptied_switch_table_demotes_to_unresolved_at_the_dispatch_addr() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let target_vn = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: 0x38,
        size: 8,
    };
    // The dispatch is the second p-code op of the instruction at 0x1004,
    // so it is NOT `at_machine_start` of the region's last instruction.
    let dispatch = addr(0x1004, 2);
    let mut dispatcher = make_region(&[(0x1000, 0), (0x1004, 2), (0x1004, 3)]);
    dispatcher.terminator = RegionTerminator::Switch {
        target_vn,
        targets: vec![crate::ResolvedTarget::new(0x2002, None)],
        addr: dispatch,
    };
    let parent = b.add_region(dispatcher).unwrap();
    // 0x2002 is interior to this region but on no instruction boundary.
    let owner = b
        .add_region(make_region(&[(0x2000, 0), (0x2008, 0)]))
        .unwrap();
    let edges_before = b.region_graph.edge_references().count();

    b.seat_non_boundary_target(parent, owner, addr(0x2002, 0))
        .unwrap();

    assert_eq!(
        b.region_graph.node_weight(parent).unwrap().terminator,
        RegionTerminator::UnresolvedIndirectBranch {
            target_vn,
            addr: dispatch,
        },
        "an emptied table demotes, keeping the dispatch varnode and address"
    );
    assert_eq!(
        b.region_graph.edge_references().count(),
        edges_before,
        "a demoted site wires no arm edge"
    );
}

/// One address seeded from two dispatch sites: the failure belongs to the
/// site that failed. Stripping by machine address takes the other site's
/// arm too, and where that empties its table the demoted
/// `UnresolvedIndirectBranch` keeps a live outgoing edge, which its
/// contract forbids.
#[test]
fn an_undecodable_arm_is_dropped_only_from_the_site_that_seeded_it() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let target_vn = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: 0x38,
        size: 8,
    };
    let shared = 0x2000u64;
    let switch_region = |dispatch| {
        let mut r = make_region(&[(dispatch, 0)]);
        r.terminator = RegionTerminator::Switch {
            target_vn,
            targets: vec![crate::ResolvedTarget::new(shared, None)],
            addr: addr(dispatch, 0),
        };
        r
    };
    let failed = b.add_region(switch_region(0x1000)).unwrap();
    let live = b.add_region(switch_region(0x1100)).unwrap();
    let arm = b.add_region(make_region(&[(shared, 0)])).unwrap();
    b.region_graph.add_edge(live, arm, ());
    // The failing site wired its arm edge before the decode that errored.
    b.region_graph.add_edge(failed, arm, ());
    b.refused_arms.push((failed, addr(shared, 0)));

    b.drop_refused_switch_arms();

    assert_eq!(
        b.region_graph.node_weight(failed).unwrap().terminator,
        RegionTerminator::UnresolvedIndirectBranch {
            target_vn,
            addr: addr(0x1000, 0),
        },
        "the seeding site loses its only arm and re-defers"
    );
    assert!(
        b.region_graph.find_edge(failed, arm).is_none(),
        "a re-deferred site keeps no outgoing edge"
    );
    assert_eq!(
        b.region_graph.node_weight(live).unwrap().terminator,
        RegionTerminator::Switch {
            target_vn,
            targets: vec![crate::ResolvedTarget::new(shared, None)],
            addr: addr(0x1100, 0),
        },
        "the other site decoded the same address in its own context"
    );
    assert!(
        b.region_graph.find_edge(live, arm).is_some(),
        "the surviving arm keeps its edge"
    );
}

/// An address seated in both ISA modes wires two parallel edges to its one
/// region; refusing the address takes both, and leaves the other arm's.
#[test]
fn a_refused_arm_seated_in_both_modes_loses_both_parallel_edges() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let target_vn = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: 0x38,
        size: 8,
    };
    let (bad, good) = (0x2000u64, 0x3000u64);
    let mut dispatcher = make_region(&[(0x1000, 0)]);
    dispatcher.terminator = RegionTerminator::Switch {
        target_vn,
        targets: vec![
            crate::ResolvedTarget::new(bad, Some(false)),
            crate::ResolvedTarget::new(bad, Some(true)),
            crate::ResolvedTarget::new(good, None),
        ],
        addr: addr(0x1000, 0),
    };
    let site = b.add_region(dispatcher).unwrap();
    let bad_arm = b.add_region(make_region(&[(bad, 0)])).unwrap();
    let good_arm = b.add_region(make_region(&[(good, 0)])).unwrap();
    b.region_graph.add_edge(site, bad_arm, ());
    b.region_graph.add_edge(site, good_arm, ());
    b.region_graph.add_edge(site, bad_arm, ());
    b.refused_arms.push((site, addr(bad, 0)));

    b.drop_refused_switch_arms();

    assert!(
        b.region_graph.find_edge(site, bad_arm).is_none(),
        "both parallel edges to the refused address go"
    );
    let survivors: Vec<_> = b.region_graph.neighbors(site).collect();
    assert_eq!(survivors, vec![good_arm], "the other arm keeps its edge");
    assert_eq!(
        b.region_graph[site].terminator,
        RegionTerminator::Switch {
            target_vn,
            targets: vec![crate::ResolvedTarget::new(good, None)],
            addr: addr(0x1000, 0),
        },
    );
}

/// Only a `Switch` resolves arms by exact start address; every other
/// terminator edges to the region that OWNS the interior address.
#[test]
fn non_switch_parent_edges_to_the_owning_region() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let parent = b.add_region(make_region(&[(0x1000, 0)])).unwrap();
    let owner = b
        .add_region(make_region(&[(0x2000, 0), (0x2008, 0)]))
        .unwrap();

    b.seat_non_boundary_target(parent, owner, addr(0x2002, 0))
        .unwrap();

    assert!(
        b.region_graph.find_edge(parent, owner).is_some(),
        "a non-Switch parent keeps its edge to the owning region"
    );
}

#[test]
fn split_at_start_is_noop() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let id = b
        .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
        .unwrap();
    let edges_before = b.region_graph.edge_references().count();
    let index_before = b.index.clone();

    let result = b.split_region(id, addr(0x1000, 0)).unwrap();
    assert_eq!(result, id);
    assert_eq!(b.region_graph.node_count(), 1);
    assert_eq!(b.region_graph.edge_references().count(), edges_before);
    assert_eq!(b.index, index_before);
}

#[test]
fn split_creates_two_regions_second_keeps_original_id() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let original = b
        .add_region(make_region(&[
            (0x1000, 0),
            (0x1004, 0),
            (0x1008, 0),
            (0x100c, 0),
        ]))
        .unwrap();
    let second = b.split_region(original, addr(0x1008, 0)).unwrap();
    assert_eq!(second, original, "second half retains original NodeIndex");
    assert_eq!(b.region_graph.node_count(), 2);
}

#[test]
fn split_second_half_is_always_non_empty() {
    // Pins what `split_region`'s `debug_assert!` defends: a real split
    // must leave the second half (which keeps `region_id` and the
    // original terminator) non-empty, so it never silently bypasses
    // `add_region`'s empty-region guard.
    for split_at in [0x1004u64, 0x1008, 0x100c] {
        let mut engine = make_engine();
        let mut b = make_builder(0x1000, &mut engine);
        let original = b
            .add_region(make_region(&[
                (0x1000, 0),
                (0x1004, 0),
                (0x1008, 0),
                (0x100c, 0),
            ]))
            .unwrap();
        let second = b.split_region(original, addr(split_at, 0)).unwrap();
        assert_eq!(second, original);
        assert!(
            !b.region_graph[second].insns.is_empty(),
            "second half empty after split at {split_at:#x}"
        );
    }
}

#[test]
fn split_produces_correct_addr_ranges() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let original = b
        .add_region(make_region(&[
            (0x1000, 0),
            (0x1004, 0),
            (0x1008, 0),
            (0x100c, 0),
        ]))
        .unwrap();
    b.split_region(original, addr(0x1008, 0)).unwrap();

    assert_eq!(b.region_graph[original].start_addr, addr(0x1008, 0));
    assert_eq!(b.region_graph[original].insns.len(), 2);

    let first_id = b.index.get(addr(0x1000, 0)).expect("indexed");
    assert_eq!(b.region_graph[first_id].start_addr, addr(0x1000, 0));
    assert_eq!(b.region_graph[first_id].insns.len(), 2);
}

#[test]
fn split_adds_fallthrough_edge() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let original = b
        .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
        .unwrap();
    b.split_region(original, addr(0x1008, 0)).unwrap();

    let edges: Vec<_> = b.region_graph.edge_references().collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].target(), original);
}

#[test]
fn split_rewires_incoming_edges_to_first_half() {
    let mut engine = make_engine();
    let mut b = make_builder(0x1000, &mut engine);
    let a = b.add_region(make_region(&[(0x0ff0, 0)])).unwrap();
    let b_id = b
        .add_region(make_region(&[(0x1000, 0), (0x1004, 0), (0x1008, 0)]))
        .unwrap();
    b.region_graph.add_edge(a, b_id, ());

    b.split_region(b_id, addr(0x1004, 0)).unwrap();

    let first = b.index.get(addr(0x1000, 0)).expect("indexed");
    let incoming: Vec<_> = b
        .region_graph
        .edges_directed(first, petgraph::Incoming)
        .collect();
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].source(), a);

    // The original `a -> b_id` edge was rewired to `a -> first`, leaving
    // the second half only the split's own fall-through from `first`.
    let second_incoming: Vec<_> = b
        .region_graph
        .edges_directed(b_id, petgraph::Incoming)
        .collect();
    assert_eq!(second_incoming.len(), 1);
    assert_eq!(second_incoming[0].source(), first);
}

/// The probe lifts a MID-INSTRUCTION address out of band, and
/// `oneInstruction` applies that constructor's `globalset`s with no
/// `RegionBuilder` to re-derive them. So it must not run where
/// `Builder::enqueue` believes a snapshot of the engine, which it does
/// exactly where this build re-derives committed context.
///
/// Asserted on the difference the guard makes, not on a `false` return:
/// the probe answers `false` for almost any address, so a test that only
/// checks `false` passes whether the guard is there or not.
#[test]
fn re_deriving_committed_context_bars_the_reentry_probe() {
    // 0x1000 je 0x1003 ; 0x1002 lock xadd %ecx,(%eax) ; mov %ecx,%eax ; ret
    const LOCK_REENTRY: [u8; 9] = [0x74, 0x01, 0xf0, 0x0f, 0xc1, 0x08, 0x89, 0xc8, 0xc3];
    let interior = |lent: Vec<(&'static str, super::FlowCommit, u32)>| {
        let arch = strider_target::SleighArch::x86();
        let reader = rsleigh::mem_readers::BufMemReader::new(LOCK_REENTRY.to_vec(), 0x1000);
        let mut engine = crate::DecodeEngine::new(arch, reader).expect("decode engine");
        let opts = crate::CfgOptions::default();
        let b = engine.builder(0x1000u64.into(), &opts).expect("builder");
        let mut tables = b.engine.arch.decode_tables().clone();
        tables.flow_commit = lent;
        b.engine.tables = Box::leak(Box::new(tables));
        b.build()
            .expect("build")
            .incomplete()
            .of(crate::Reason::InteriorTarget)
            .map(|a| a.machine())
            .collect::<Vec<_>>()
    };
    assert!(
        interior(Vec::new()).is_empty(),
        "with nothing to re-derive the probe runs and the reentry is exact",
    );
    // `segover` is a real x86 flowing var (`ia.sinc:411`) that no
    // constructor commits, standing in for an arch that does commit one:
    // what is under test is the guard, not this var. It has to exist in
    // the sla, because `rederive_flow` reads every name it is lent.
    assert_eq!(
        interior(vec![("segover", super::FlowCommit::Flowing, 0)]),
        vec![0x1003],
        "with committed context to re-derive the probe must be barred, so the \
         reentry falls back to being reported as interior",
    );
}

/// The engine reads its space table once: a build adds no address space, so
/// the table it hands every `Cfg` is the Sleigh's own.
#[test]
fn the_engine_space_table_is_the_sleighs_after_a_build() {
    let mut engine = crate::test_support::make_engine_over(vec![0x8b, 0x07, 0xc3], 0x1000);
    let cfg = engine
        .build_cfg(0x1000u64.into(), &crate::CfgOptions::default())
        .expect("build");
    assert_eq!(engine.space_ids(), &engine.sleigh().space_ids());
    assert_eq!(cfg.space_ids(), engine.space_ids());
}

/// The `InteriorTarget` report of each x86 `lock cmpxchg` re-entry shape,
/// under every one of `seeds` visit orders.
fn lock_reentry_reports(bytes: &[u8], seeds: std::ops::RangeInclusive<u64>) -> Vec<Vec<u64>> {
    let mut seen = Vec::new();
    for seed in seeds {
        super::SHUFFLE_SEED.with(|s| s.set(Some(seed)));
        let cfg = crate::DecodeEngine::new(
            strider_target::SleighArch::x86(),
            rsleigh::mem_readers::BufMemReader::new(bytes.to_vec(), 0x1000),
        )
        .expect("decode engine")
        .build_cfg(0x1000u64.into(), &crate::CfgOptions::default())
        .expect("build");
        let interior: Vec<u64> = cfg
            .incomplete()
            .of(crate::Reason::InteriorTarget)
            .map(PcodeInsnAddr::machine)
            .collect();
        if !seen.contains(&interior) {
            seen.push(interior);
        }
    }
    super::SHUFFLE_SEED.with(|s| s.set(None));
    seen
}

/// A branch one byte into a `lock cmpxchg` heading its region re-enters the
/// same instruction, whichever slice of the `cmpxchg` decodes first.
///
/// ```text
/// lock loop:  1000 lock cmpxchg; 1004 jne 0x1001; 1006 ret
/// glibc:      1000 je 0x1003; 1002 lock cmpxchg; 1006 jne 0x1003; 1008 ret
/// ```
#[test]
fn a_lock_reentry_report_is_the_same_in_every_visit_order() {
    for bytes in [
        &[0xf0, 0x0f, 0xb1, 0x0f, 0x75, 0xfb, 0xc3][..],
        &[0x74, 0x01, 0xf0, 0x0f, 0xb1, 0x0f, 0x75, 0xfb, 0xc3],
    ] {
        assert_eq!(lock_reentry_reports(bytes, 1..=64), [Vec::<u64>::new()]);
    }
}

/// The `jne` to 0x1004 splits the `xor` region at the `lock` instruction,
/// so the `jmp` one byte into it is exact once both have decoded.
///
/// ```text
/// 1000 jne 0x1009; 1002 xor eax, eax; 1004 lock cmpxchg; 1008 ret
/// 1009 jne 0x1004; 100b jmp 0x1005
/// ```
#[test]
fn a_lock_reentry_judged_before_the_split_at_its_head_is_not_reported() {
    let bytes = [
        0x75, 0x07, 0x31, 0xc0, 0xf0, 0x0f, 0xb1, 0x0f, 0xc3, 0x75, 0xf9, 0xeb, 0xf8,
    ];
    assert_eq!(lock_reentry_reports(&bytes, 1..=64), [Vec::<u64>::new()]);
}
