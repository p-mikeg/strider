//! `Builder::build` driven by hand-crafted byte sequences, covering the
//! boundary shapes a real binary rarely reaches.

use rustc_hash::FxHashMap;

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions, PcodeInsnAddr, RegionTerminator, ResolvedTargets};
use strider_target::SleighArch;

type TestReader = BufMemReader<Vec<u8>>;

fn make_sleigh_x86_64(bytes: Vec<u8>, base: u64) -> Sleigh<TestReader> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh x86_64")
}

fn build_from_bytes(bytes: Vec<u8>, start: u64) -> Cfg {
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, start);
    Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
        .build()
        .expect("Builder::build on synthetic bytes")
}

fn build_from_bytes_opts(bytes: Vec<u8>, start: u64, opts: &CfgOptions) -> Cfg {
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, start);
    Builder::for_arch(&arch, &mut sleigh, start, opts)
        .build()
        .expect("Builder::build on synthetic bytes")
}

fn build_from_bytes_ccs(
    bytes: Vec<u8>,
    start: u64,
    per_address_ccs: &FxHashMap<u64, strider_target::BuiltCallingConvention>,
) -> Cfg {
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, start);
    Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
        .with_per_address_ccs(per_address_ccs.clone())
        .build()
        .expect("Builder::build on synthetic bytes")
}

/// Flags each `target` address `no_return`.
fn no_return_ccs(targets: &[u64]) -> FxHashMap<u64, strider_target::BuiltCallingConvention> {
    targets
        .iter()
        .map(|&a| {
            (
                a,
                strider_target::BuiltCallingConvention {
                    no_return: true,
                    ..Default::default()
                },
            )
        })
        .collect()
}

/// A `no_return` call terminates its region even MID-function, where the
/// return address is in-bounds and the structural function-end fallback cannot
/// fire.  Without the flag the same call falls through to the trailing `ret`.
#[test]
fn direct_call_to_no_return_target_terminates_region_mid_function() {
    // 0x1000: call 0x2005, a far target chosen so it cannot coincide with
    //         the fall-through and seed a split
    // 0x1005: xor eax,eax   (31 c0)
    // 0x1007: ret           (c3)
    let bytes = vec![0xe8, 0x00, 0x10, 0x00, 0x00, 0x31, 0xc0, 0xc3];

    // Baseline: an ordinary mid-function call falls through to the `ret`.
    let cfg = build_from_bytes(bytes.clone(), 0x1000);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::Return,
        "unmarked mid-function call falls through to the ret"
    );

    // Flagged `no_return`: the region now ends AT the call.
    let cfg = build_from_bytes_ccs(bytes, 0x1000, &no_return_ccs(&[0x2005]));
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn,
        "a call to a no_return-flagged target terminates the region"
    );
}

#[test]
fn single_ret_produces_one_region_without_tail_call_flag() {
    let cfg = build_from_bytes(vec![0xc3], 0x1000);
    assert_eq!(cfg.region_graph().node_count(), 1);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::Return
    );
}

#[test]
fn back_jump_splits_region() {
    // The `jmp -4` targets 0x1002, mid-region, triggering split_region.
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = build_from_bytes(bytes, 0x1000);
    assert!(
        cfg.region_graph().node_count() >= 2,
        "expected at least 2 regions after back-jump split; got {}",
        cfg.region_graph().node_count()
    );
    // The back-jump edges from the branch region to the split second half.
    assert!(
        cfg.region_graph().edge_count() >= 1,
        "expected at least one edge from the back-jump"
    );
}

#[test]
fn split_both_halves_unconditional() {
    // Same back-jump fixture, asserting per-half terminators.
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = build_from_bytes(bytes, 0x1000);

    let mut first_half = None;
    let mut second_half = None;
    for r in cfg.region_graph().node_weights() {
        if r.start_addr.machine_addr.addr == 0x1000 {
            first_half = Some(r);
        } else if r.start_addr.machine_addr.addr == 0x1002 {
            second_half = Some(r);
        }
    }
    let first_half = first_half.expect("first half (0x1000) region");
    let second_half = second_half.expect("second half (0x1002) region");

    assert_eq!(first_half.terminator, RegionTerminator::Unconditional);
    assert_eq!(second_half.terminator, RegionTerminator::Unconditional);
}

#[test]
fn fn_max_size_forces_forward_jump_to_be_tail_call() {
    // With fn_max_size=0x10 the target 0x1012 is past the bound.
    let bytes = vec![0xeb, 0x10];
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
    assert_eq!(cfg.region_graph().node_count(), 1);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::TailCall {
            target: 0x1012.into()
        }
    );
}

#[test]
fn forward_jump_landing_exactly_at_fn_max_size_is_tail_call() {
    // Target 0x1010 lands EXACTLY on `start + fn_max_size`.  The window is
    // half-open, so that is out of bounds and must be a tail call.  A `ret`
    // sits at 0x1010 so the bytes would decode if the edge were followed.
    let mut bytes = vec![0xeb, 0x0e]; // jmp +0x0e (next insn 0x1002 + 0x0e = 0x1010)
    bytes.extend(std::iter::repeat_n(0x90u8, 14)); // nop filler to 0x1010
    bytes.push(0xc3); // 0x1010: ret
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
    assert_eq!(cfg.region_graph().node_count(), 1);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::TailCall {
            target: 0x1010.into()
        },
        "addr == start + fn_max_size must already be out-of-range (half-open bound)"
    );
}

#[test]
fn forward_jump_landing_just_inside_fn_max_size_is_followed() {
    // Companion probe one byte lower, strictly inside: the edge must be
    // followed and the target region decoded.
    let mut bytes = vec![0xeb, 0x0d]; // jmp +0x0d -> 0x1002 + 0x0d = 0x100f
    bytes.extend(std::iter::repeat_n(0x90u8, 13)); // nop filler to 0x100f
    bytes.push(0xc3); // 0x100f: ret
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
    assert!(
        !matches!(
            cfg.region_graph()[cfg.entry()].terminator,
            RegionTerminator::TailCall { .. }
        ),
        "target strictly inside the bound must be followed, not tail-called; got {:?}",
        cfg.region_graph()[cfg.entry()].terminator
    );
    assert!(
        cfg.region_graph().node_count() >= 2,
        "followed in-range jump must decode the target region"
    );
    assert!(
        cfg.regions()
            .any(|r| r.terminator == RegionTerminator::Return),
        "the ret at 0x100f must be decoded as a Return region"
    );
}

#[test]
fn allow_code_before_start_addr_negates_below_start_tail_call() {
    // `ret` below the function start; `jmp -16` from 0x1000 reaches 0x0ff2.
    let mut bytes = vec![0u8; 0x14];
    bytes[0x02] = 0xc3; // 0x0ff2: ret
    bytes[0x10] = 0xeb; // 0x1000: jmp
    bytes[0x11] = 0xf0; // rel8 = -16 -> 0x0ff2

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, 0x0ff0);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = CfgOptions {
        allow_code_before_start_addr: true,
        ..CfgOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh, 0x1000, &opts)
        .build()
        .unwrap();

    assert_ne!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::TailCall {
            target: 0x0ff2.into()
        },
        "entry region must NOT be a TailCall when allow_code_before_start_addr is set"
    );
    assert!(
        cfg.region_graph().edge_count() >= 1,
        "expected at least one edge since the below-start target is followed"
    );
}

/// Asserts the region at `addr` is a tail-call stub: zero instructions, a
/// `TailCall` back to its own start address, and no outgoing edge.
fn assert_tail_call_stub_at(cfg: &Cfg, addr: u64) {
    let (id, stub) = cfg
        .region_ids()
        .map(|id| (id, &cfg.region_graph()[id]))
        .find(|(_, r)| r.start_addr == PcodeInsnAddr::at_machine_start(addr))
        .unwrap_or_else(|| panic!("expected a stub region at {addr:#x}"));
    assert_eq!(
        stub.terminator,
        RegionTerminator::TailCall {
            target: addr.into()
        },
        "stub at {addr:#x} must terminate as TailCall to its own address"
    );
    assert!(
        stub.insns.is_empty(),
        "stub at {addr:#x} is synthetic, so the OOB bytes must never be decoded"
    );
    assert_eq!(
        cfg.region_graph()
            .edges_directed(id, petgraph::Outgoing)
            .count(),
        0,
        "stub at {addr:#x} is a sink: a TailCall has no successor edge"
    );
}

#[test]
fn cond_branch_with_oob_fallthrough_keeps_cond_branch_with_tail_call_stub() {
    // Taken 0x1000 is in range; the fall-through 0x1004 is out of bounds at
    // fn_max_size=4.  The conditional survives, with the OOB arm lowered to a
    // stub wired as a regular CondBranch successor.
    let bytes = vec![0x31u8, 0xc0, 0x74, 0xfc];
    let opts = CfgOptions {
        fn_max_size: Some(4),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::CondBranch {
            true_target: PcodeInsnAddr::at_machine_start(0x1000)
        },
        "entry region must retain CondBranch when one successor is OOB"
    );
    assert_tail_call_stub_at(&cfg, 0x1004);
    // Entry (taken arm self-loops to its own start) plus the stub; edges are
    // the self-loop and the stub edge.
    assert_eq!(cfg.region_graph().node_count(), 2);
    assert_eq!(cfg.region_graph().edge_count(), 2);
}

#[test]
fn cond_branch_with_oob_taken_target_keeps_cond_branch_with_tail_call_stub() {
    // Mirror of the OOB-fall-through test: here the TAKEN target 0x107e is
    // out of bounds and the fall-through 0x1004 is in range.
    let bytes = vec![0x31u8, 0xc0, 0x74, 0x7a, 0xc3];
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::CondBranch {
            true_target: PcodeInsnAddr::at_machine_start(0x107e)
        },
        "entry region must retain CondBranch when the taken successor is OOB"
    );
    assert_tail_call_stub_at(&cfg, 0x107e);
    // Entry, the in-range fall-through, and the stub; one edge per arm.
    assert_eq!(cfg.region_graph().node_count(), 3);
    assert_eq!(cfg.region_graph().edge_count(), 2);
    let fallthrough = cfg
        .region_graph()
        .node_weights()
        .find(|r| r.start_addr.machine_addr.addr == 0x1004)
        .expect("fall-through region at 0x1004 must be decoded");
    assert_eq!(fallthrough.terminator, RegionTerminator::Return);
}

#[test]
fn cond_branch_with_both_targets_oob_keeps_cond_branch_with_two_stubs() {
    // With fn_max_size=2 both arms are out of bounds.  The conditional
    // survives with both lowered to stubs; collapsing to a single TailCall
    // would silently drop the fall-through arm.
    let bytes = vec![0x74u8, 0x7e];
    let opts = CfgOptions {
        fn_max_size: Some(2),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::CondBranch {
            true_target: PcodeInsnAddr::at_machine_start(0x1080)
        },
        "entry region must retain CondBranch when both successors are OOB"
    );
    assert_tail_call_stub_at(&cfg, 0x1080);
    assert_tail_call_stub_at(&cfg, 0x1002);
    assert_eq!(cfg.region_graph().node_count(), 3);
    assert_eq!(cfg.region_graph().edge_count(), 2);
}

#[test]
fn cond_branches_to_same_oob_target_share_one_stub() {
    // Two conditional branches to the SAME OOB address 0x1080.  Its stub must
    // be created once and shared: regions are keyed by start address, so a
    // second stub would collide.
    let bytes = vec![0x74u8, 0x7e, 0x75, 0x7c, 0xc3];
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    let stub_count = cfg
        .regions()
        .filter(|r| matches!(r.terminator, RegionTerminator::TailCall { .. }))
        .count();
    assert_eq!(stub_count, 1, "both branches must share one stub region");
    assert_tail_call_stub_at(&cfg, 0x1080);
    // Regions: je, jne, ret, shared stub.  Edges: je to stub, je to jne, jne
    // to stub, jne to ret.
    assert_eq!(cfg.region_graph().node_count(), 4);
    assert_eq!(cfg.region_graph().edge_count(), 4);
    let stub_id = cfg
        .region_ids()
        .find(|&id| {
            matches!(
                cfg.region_graph()[id].terminator,
                RegionTerminator::TailCall { .. }
            )
        })
        .expect("stub region");
    assert_eq!(
        cfg.region_graph()
            .edges_directed(stub_id, petgraph::Incoming)
            .count(),
        2,
        "both cond-branch regions must wire their OOB arm to the shared stub"
    );
}

#[test]
fn fall_through_past_fn_max_size_is_function_boundary_error() {
    // A `lock cmpxchg` is multi-pcode-op with intra-insn CONST branches.
    // Running off fn_max_size=2 with no explicit terminator opcode is a
    // function-boundary error, not a tail call.
    let mut bytes = vec![0x31u8, 0xc0];
    bytes.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);
    let opts = CfgOptions {
        fn_max_size: Some(2),
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, 0x1000);
    let err = Builder::for_arch(&arch, &mut sleigh, 0x1000, &opts)
        .build()
        .expect_err(
            "sequential fall-through past fn_max_size must error, not classify as tail call",
        );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("function-boundary error"),
        "expected function-boundary error message; got {msg}"
    );
    assert!(
        msg.contains("sequential decoding overflowed"),
        "expected overflow detail in error message; got {msg}"
    );
}

#[test]
fn fall_through_single_insn_past_fn_max_size_is_function_boundary_error() {
    // `mov eax, 5` starts inside fn_max_size=3 but falls through to 0x1005,
    // past the 0x1003 bound, with no terminator opcode.  Must be a
    // function-boundary error, not a synthetic tail call.  This is the
    // `tzcount.o` shape: a small function with no terminator inside its
    // recorded bound.
    let bytes = vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90];
    let opts = CfgOptions {
        fn_max_size: Some(3),
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, 0x1000);
    let err = Builder::for_arch(&arch, &mut sleigh, 0x1000, &opts)
        .build()
        .expect_err("single-insn fall-through past fn_max_size must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("function-boundary error"),
        "expected function-boundary error message; got {msg}"
    );
    assert!(
        msg.contains("sequential decoding overflowed"),
        "expected overflow detail in error message; got {msg}"
    );
}

#[test]
fn probe_x86_nop_lifts_to_zero_pcode_ops() {
    // Precondition for the zero-pcode-prefix test below: x86 `nop` lifts to
    // zero pcode ops here, so a run of them never appends to
    // `RegionBuilder::insns`.
    let mut sleigh = make_sleigh_x86_64(vec![0x90u8], 0x1000);
    let lift = sleigh.lift_one(0x1000).expect("lift_one nop");
    assert!(
        lift.insns.is_empty(),
        "expected x86 nop to lift to zero pcode ops; got {}",
        lift.insns.len()
    );
}

#[test]
fn zero_pcode_prefix_crossing_fn_max_size_is_function_boundary_error() {
    // Zero-pcode nops run across the bound with a real `ret` past it.  Since
    // they produce no pcode ops, `insns` stays empty while decode walks
    // machine-by-machine past `start + fn_max_size`, so the boundary check
    // must fire on the first past-bound instruction rather than absorb the
    // next function's `ret`.
    //
    // The bound is 0x1002; three nops carry decode to 0x1003 with `insns`
    // still empty, and the `ret` there belongs to the next function.
    let bytes = vec![0x90u8, 0x90, 0x90, 0xc3];
    let opts = CfgOptions {
        fn_max_size: Some(2),
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, 0x1000);
    let err = Builder::for_arch(&arch, &mut sleigh, 0x1000, &opts)
        .build()
        .expect_err(
            "zero-pcode prefix crossing fn_max_size must error, not absorb the next function",
        );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("function-boundary error"),
        "expected function-boundary error message; got {msg}"
    );
    assert!(
        msg.contains("sequential decoding overflowed"),
        "expected overflow detail in error message; got {msg}"
    );
}

#[test]
fn fn_max_size_smaller_than_first_terminator_insn_still_builds_tail_call() {
    // A 2-byte `jmp +0x10` under fn_max_size=1 starts in range but its
    // encoding crosses the bound.  Decoding is NOT length-bounded: the
    // instruction decodes fully and its OOB target is a tail call.  Contrast
    // the erroring cases above; the bound trips only on sequential
    // fall-through, never mid-instruction.
    let bytes = vec![0xebu8, 0x10]; // jmp +0x10 -> target 0x1012
    let opts = CfgOptions {
        fn_max_size: Some(1),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
    assert_eq!(cfg.region_graph().node_count(), 1);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::TailCall {
            target: 0x1012.into()
        }
    );
}

#[test]
fn jump_to_entry_address_forms_single_region_self_loop() {
    // `jmp -2` targets the entry address itself.  No split, since the target
    // IS the region start rather than a mid-region address.
    let bytes = vec![0xebu8, 0xfe]; // jmp -2 -> 0x1000
    let cfg = build_from_bytes(bytes, 0x1000);
    assert_eq!(cfg.region_graph().node_count(), 1);
    assert_eq!(
        cfg.region_graph().edge_count(),
        1,
        "the back-edge to entry is a self-edge"
    );
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::Unconditional
    );
    assert_eq!(
        cfg.region_graph()[cfg.entry()].start_addr.machine_addr.addr,
        0x1000
    );
}

#[test]
fn ud2_region_finishes_as_noreturn() {
    // Sleigh lifts `ud2` to a CallOther (invalidInstructionException)
    // followed by a BranchIndirect.  The region must terminate at the
    // CallOther, before the trailing BranchIndirect can route.
    let bytes = vec![0x0fu8, 0x0b];
    let cfg = build_from_bytes(bytes, 0x1000);

    let any_noreturn = cfg
        .region_graph()
        .node_weights()
        .any(|r| matches!(r.terminator, RegionTerminator::NoReturn));
    assert!(
        any_noreturn,
        "expected at least one NoReturn region; got terminators: {:?}",
        cfg.region_graph()
            .node_weights()
            .map(|r| &r.terminator)
            .collect::<Vec<_>>()
    );
    let any_unresolved = cfg.region_graph().node_weights().any(|r| {
        matches!(
            r.terminator,
            RegionTerminator::UnresolvedIndirectBranch { .. }
        )
    });
    assert!(
        !any_unresolved,
        "trap region should terminate as NoReturn before its trailing BranchIndirect routes"
    );
}

fn build_ppc32be(words: &[u32], start: u64, opts: &CfgOptions) -> Cfg {
    let arch = SleighArch::ppc32be();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    let reader = BufMemReader::new(bytes, start);
    let mut sleigh =
        Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh ppc32be");
    Builder::for_arch(&arch, &mut sleigh, start, opts)
        .build()
        .expect("Builder::build on synthetic ppc words")
}

/// `li r3,1` / the trap / `li r3,2` / `blr`, as (sealed at the trap, addresses
/// decoded).
fn ppc_trap_run(trap_word: u32, opts: &CfgOptions) -> (bool, Vec<u64>) {
    const LI_R3_1: u32 = 0x3860_0001;
    const LI_R3_2: u32 = 0x3860_0002;
    const BLR: u32 = 0x4e80_0020;
    let cfg = build_ppc32be(&[LI_R3_1, trap_word, LI_R3_2, BLR], 0x1000, opts);
    let sealed = cfg
        .region_graph()
        .node_weights()
        .any(|r| r.terminator == RegionTerminator::NoReturn);
    let mut decoded: Vec<u64> = cfg
        .region_graph()
        .node_weights()
        .flat_map(|r| r.insns.iter().map(|i| i.addr.machine_addr.addr))
        .collect();
    decoded.sort_unstable();
    decoded.dedup();
    (sealed, decoded)
}

/// A `twi` naming every TO relation is `BUG()`: it seals the region and the
/// fall-through is never decoded, while a narrower mask is a conditional check
/// the region continues past.  A caller override for the name answers instead
/// of both, since it states what this binary's build of the op does.
#[test]
fn a_call_other_override_outranks_the_unconditional_ppc_trap_rule() {
    // 1004: 0f e3 00 00    twi 31,r3,0
    const TWI_ALL_RELATIONS: u32 = 0x0fe3_0000;
    // 1004: 0c 83 00 00    twi 4,r3,0
    const TWI_ONE_RELATION: u32 = 0x0c83_0000;

    let (sealed, decoded) = ppc_trap_run(TWI_ALL_RELATIONS, &CfgOptions::default());
    assert!(sealed, "TO=31 is unconditional, so the region ends there");
    assert_eq!(
        decoded,
        vec![0x1000, 0x1004],
        "nothing past an unconditional trap is decoded"
    );

    let (sealed, decoded) = ppc_trap_run(TWI_ONE_RELATION, &CfgOptions::default());
    assert!(!sealed, "TO=4 traps on one relation, so control continues");
    assert_eq!(decoded, vec![0x1000, 0x1004, 0x1008, 0x100c]);

    let opts = CfgOptions {
        call_other_overrides: strider_target::call_other_abi::CallOtherOverrides::new(vec![(
            "trapWord".to_owned(),
            strider_target::call_other_abi::CallOtherClass::PURE.into(),
        )])
        .expect("unique override names"),
        ..CfgOptions::default()
    };
    let (sealed, decoded) = ppc_trap_run(TWI_ALL_RELATIONS, &opts);
    assert!(
        !sealed,
        "a returning override for `trapWord` must keep the region open"
    );
    assert_eq!(decoded, vec![0x1000, 0x1004, 0x1008, 0x100c]);
}

fn build_one(mut sleigh: Sleigh<TestReader>, start: u64) -> (Cfg, Sleigh<TestReader>) {
    let arch = SleighArch::x86_64();
    let cfg = Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
        .build()
        .expect("Builder::build");
    (cfg, sleigh)
}

#[test]
fn cfg_build_returns_sleigh_for_reuse() {
    let bytes = vec![0xc3u8];
    let sleigh = make_sleigh_x86_64(bytes, 0x1000);

    // The Cfg never owns the Sleigh, so a rebuild reuses it without
    // re-loading the SLA spec.
    let (cfg1, sleigh) = build_one(sleigh, 0x1000);
    assert!(cfg1.region_graph().node_count() >= 1);

    let (cfg2, _sleigh) = build_one(sleigh, 0x1000);
    assert!(cfg2.region_graph().node_count() >= 1);
}

#[test]
fn sleigh_can_be_used_for_multiple_cfg_builds() {
    let bytes_a = vec![0xc3u8];
    let sleigh = make_sleigh_x86_64(bytes_a, 0x1000);
    let (cfg1, sleigh) = build_one(sleigh, 0x1000);
    let count1 = cfg1.region_graph().node_count();
    let (cfg2, sleigh) = build_one(sleigh, 0x1000);
    let count2 = cfg2.region_graph().node_count();
    let (cfg3, _final_sleigh) = build_one(sleigh, 0x1000);
    assert!(count1 >= 1);
    assert!(count2 >= 1);
    assert!(cfg3.region_graph().node_count() >= 1);
    let _ = cfg3.entry();
}

fn build_unresolved_jmp_rax_cfg() -> Cfg {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    Builder::for_arch(&arch, &mut sleigh, base, &CfgOptions::default())
        .build()
        .expect("build")
}

fn locate_unresolved_addr(cfg: &Cfg) -> PcodeInsnAddr {
    for region_id in cfg.region_ids() {
        let region = cfg.region_graph().node_weight(region_id).expect("region");
        if let RegionTerminator::UnresolvedIndirectBranch { addr, .. } = &region.terminator {
            return *addr;
        }
    }
    panic!("CFG has no UnresolvedIndirectBranch region");
}

#[test]
fn with_known_targets_default_is_unresolved() {
    let cfg = build_unresolved_jmp_rax_cfg();
    let addr = locate_unresolved_addr(&cfg);
    assert!(addr.machine_addr.addr >= 0x1000);
}

#[test]
fn with_known_targets_link_register_overrides_to_return() {
    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::LinkRegister);

    let opts = CfgOptions {
        known_targets: known,
        ..CfgOptions::default()
    };
    let cfg_v2 = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with known_targets");

    let mut had_return = false;
    for region in cfg_v2.regions() {
        if matches!(region.terminator, RegionTerminator::Return) {
            had_return = true;
        }
        assert!(
            !matches!(
                region.terminator,
                RegionTerminator::UnresolvedIndirectBranch { .. }
            ),
            "with_known_targets must override UnresolvedIndirectBranch"
        );
    }
    assert!(had_return, "expected at least one Return region");
}

#[test]
fn with_known_targets_empty_map_falls_through_to_tier_1() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");

    let opts = CfgOptions {
        known_targets: FxHashMap::default(),
        ..CfgOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with empty known_targets");

    let had_unresolved = cfg.regions().any(|r| {
        matches!(
            r.terminator,
            RegionTerminator::UnresolvedIndirectBranch { .. }
        )
    });
    assert!(had_unresolved);
}

#[test]
fn known_multiple_with_out_of_range_target_defers_to_unresolved() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        unresolved_addr,
        ResolvedTargets::Multiple(vec![0x1004, 0x9000].into_iter().map(Into::into).collect()),
    );

    let opts = CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..CfgOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build must succeed; mixed Multiple defers via UnresolvedIndirectBranch");

    let mut had_unresolved = false;
    let mut had_switch = false;
    for region in cfg.regions() {
        match &region.terminator {
            RegionTerminator::UnresolvedIndirectBranch { .. } => had_unresolved = true,
            RegionTerminator::Switch { .. } => had_switch = true,
            _ => {}
        }
    }
    assert!(
        had_unresolved && !had_switch,
        "Multiple with an OOB target must defer via UnresolvedIndirectBranch, not emit a Switch"
    );
}

/// A `Single` resolution pointing outside the function range must become a
/// `TailCall`: no successor edge, no `UnresolvedIndirectBranch`.
#[test]
fn known_single_oob_target_produces_tail_call() {
    // `jmp rax` resolved to 0x9000, outside [0x1000, 0x1100).
    let base = 0x1000u64;
    let oob_target = 0x9000u64;
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::Single(oob_target.into()));

    let opts = CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..CfgOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with Single(oob) known_target must succeed");

    let mut had_tail_call = false;
    for region in cfg.regions() {
        assert!(
            !matches!(
                region.terminator,
                RegionTerminator::UnresolvedIndirectBranch { .. }
            ),
            "Single(oob) known_target must not leave UnresolvedIndirectBranch"
        );
        if let RegionTerminator::TailCall { target, .. } = region.terminator {
            assert_eq!(
                target.addr, oob_target,
                "TailCall must point at the resolved oob target"
            );
            had_tail_call = true;
        }
    }
    assert!(
        had_tail_call,
        "Single(oob) known_target must produce a TailCall terminator"
    );
}

#[test]
fn known_multiple_in_range_targets_produces_switch() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0x90u8, 32));
    bytes.push(0xc3);
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        unresolved_addr,
        ResolvedTargets::Multiple(vec![0x1004, 0x1008].into_iter().map(Into::into).collect()),
    );

    let opts = CfgOptions {
        fn_max_size: Some(0x100),
        known_targets: known,
        ..CfgOptions::default()
    };
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with in-range Multiple must succeed");

    let had_switch = cfg
        .regions()
        .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. }));
    assert!(
        had_switch,
        "in-range Multiple must produce a Switch terminator"
    );
}

/// A jump table over-approximated by one entry can land inside another arm's
/// instruction. Both arms belong to the SAME switch, so neither has a region
/// when the site is sealed and the seal-time interior guard cannot fire; only
/// exploring the real arm first lets the over-read arm be recognised as
/// interior and dropped.
///
/// `jmp rax` at 0x1000, a 10-byte `movabs rax, imm64` at 0x1002, and 0x1005
/// pointing into the middle of that immediate.
#[test]
fn a_switch_arm_interior_to_a_sibling_arm_is_dropped() {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // jmp rax
    // movabs rax, 0x1122334455667788
    bytes.extend_from_slice(&[0x48, 0xb8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    bytes.push(0xc3); // ret at 0x100c
    bytes.push(0xc3); // pad so 0x1005's decode has bytes to read

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let site = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        site,
        ResolvedTargets::Multiple(vec![0x1002.into(), 0x1005.into()]),
    );
    let opts = CfgOptions {
        known_targets: known,
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, base);
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with an over-read switch arm");

    let starts: Vec<u64> = cfg
        .regions()
        .map(|r| r.start_addr.machine_addr.addr)
        .collect();
    assert!(
        !starts.contains(&0x1005),
        "0x1005 is inside the immediate of the instruction at 0x1002, so it must \
         not decode as its own region; got starts {starts:#x?}",
    );

    for region in cfg.regions() {
        if let RegionTerminator::Switch { targets, .. } = &region.terminator {
            assert!(
                !targets.iter().any(|t| t.addr == 0x1005),
                "0x1005 is interior to the instruction at 0x1002 and must be dropped",
            );
        }
    }
}

/// A region that starts INSIDE another region's last instruction shadows it in
/// the start-address index: it is the greatest start at or below every address
/// in that instruction's tail, so a lookup that confirms containment on the
/// last start alone reports those addresses unowned.
///
/// Two `jmp rax` sites. Site A's table over-reads to 0x1005, inside the
/// immediate of the 10-byte `movabs` at 0x1002, seeding the shadow region.
/// Site B then over-reads to 0x1008, in the same immediate.
#[test]
fn a_switch_arm_shadowed_by_an_overlapping_region_is_dropped() {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // 0x1000: jmp rax (site A)
    // 0x1002: movabs rax, 0x1122c34455c36677; 0x1005 and 0x1008 each start a
    // two-byte `ret` inside the immediate (`66 c3`, `44 c3`), so a shadow
    // region can decode there.
    bytes.extend_from_slice(&[0x48, 0xb8, 0x77, 0x66, 0xc3, 0x55, 0x44, 0xc3, 0x22, 0x11]);
    bytes.push(0xc3); // 0x100c: ret
    bytes.resize(0x1000, 0xcc);
    bytes.extend_from_slice(&[0xff, 0xe0]); // 0x2000: jmp rax (site B)
    bytes.push(0xc3);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(0x1000),
        ResolvedTargets::Multiple(vec![0x1005.into(), 0x2000.into()]),
    );
    known.insert(
        PcodeInsnAddr::at_machine_start(0x2000),
        ResolvedTargets::Multiple(vec![0x1002.into(), 0x1008.into()]),
    );
    let opts = CfgOptions {
        known_targets: known,
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, base);
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with an over-read switch arm shadowed by an overlapping region");

    let starts: Vec<u64> = cfg
        .regions()
        .map(|r| r.start_addr.machine_addr.addr)
        .collect();
    assert!(
        !starts.contains(&0x1008),
        "0x1008 is inside the immediate of the instruction at 0x1002, so it must \
         not decode as its own region; got starts {starts:#x?}",
    );
    for region in cfg.regions() {
        if let RegionTerminator::Switch { targets, .. } = &region.terminator {
            assert!(
                !targets.iter().any(|t| t.addr == 0x1008),
                "0x1008 is interior to the instruction at 0x1002 and must be dropped",
            );
        }
    }
}

/// An empty region sealed at a zero-pcode-op instruction owns that
/// instruction's BYTES, so an over-read arm landing in them is interior.
///
/// `jmp rax` at 0x1000 and the four-byte `endbr64` at 0x1002, whose own region
/// carries no instruction; 0x1005 is its last byte.
#[test]
fn a_switch_arm_inside_a_zero_pcode_op_instruction_is_dropped() {
    let base = 0x1000u64;
    let mut bytes = vec![0xffu8, 0xe0]; // 0x1000: jmp rax
    bytes.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // 0x1002: endbr64
    bytes.push(0xc3); // 0x1006: ret
    bytes.push(0xc3); // pad so 0x1005's decode has bytes to read

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(base),
        ResolvedTargets::Multiple(vec![0x1002.into(), 0x1005.into()]),
    );
    let opts = CfgOptions {
        known_targets: known,
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, base);
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("build with an over-read switch arm inside a zero-pcode-op instruction");

    let starts: Vec<u64> = cfg
        .regions()
        .map(|r| r.start_addr.machine_addr.addr)
        .collect();
    assert!(
        !starts.contains(&0x1005),
        "0x1005 is the last byte of the endbr64 at 0x1002, so it must not decode \
         as its own region; got starts {starts:#x?}",
    );
    for region in cfg.regions() {
        if let RegionTerminator::Switch { targets, .. } = &region.terminator {
            assert!(
                !targets.iter().any(|t| t.addr == 0x1005),
                "0x1005 is interior to the endbr64 at 0x1002 and must be dropped",
            );
        }
        if region.start_addr.machine_addr.addr == 0x1002 {
            assert_eq!(
                (region.insns.len(), region.empty_span_len),
                (0, 4),
                "the endbr64 region carries no instruction, so its span is the \
                 recorded instruction length",
            );
        }
    }
}

/// A Thumb `IT` commits every `condit` bit at `inst_next`, so a later round
/// seating a target there re-imposes a context whose diff touches several of
/// them. The build must carry on rather than reject the context.
///
/// `bx r0 ; itte eq ; movs r0,#0 x3 ; bx lr`, seating 0x1002 in round one and
/// 0x1004 in round two, which is the orchestrator's re-lift shape.
#[test]
fn re_imposing_a_context_over_an_it_block_builds() {
    use strider_cfg::{FlowContext, FlowVars};

    let arch = SleighArch::arm_thumb();
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x0c].copy_from_slice(&[
        0x00, 0x47, 0x06, 0xbf, 0x00, 0x20, 0x00, 0x20, 0x00, 0x20, 0x70, 0x47,
    ]);
    let reader = BufMemReader::new(bytes, 0x1000);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
    let flow = FlowVars::discover(&sleigh).expect("discover flow vars");
    let defaults = flow.snapshot(&sleigh, 0x1000);

    let mut round = |target: u64| -> usize {
        flow.reset_at(&mut sleigh, 0x1000, &defaults)
            .expect("reset entry context");
        let (var, value) = arch
            .entry_mode_context(0x1001)
            .expect("ARM declares an ISA-mode var");
        sleigh
            .set_context_at(0x1000, var, value)
            .expect("pin the entry mode");
        let function_mode: FlowContext = flow.snapshot(&sleigh, 0x1000);
        let mut known = FxHashMap::default();
        known.insert(
            PcodeInsnAddr::at_machine_start(0x1000),
            ResolvedTargets::Multiple(vec![target.into()]),
        );
        let opts = CfgOptions {
            known_targets: known,
            ..CfgOptions::default()
        };
        Builder::for_arch(&arch, &mut sleigh, 0x1000, &opts)
            .with_flow_context(&flow, function_mode)
            .build()
            .expect("re-imposing a context over an IT block must build")
            .regions()
            .count()
    };

    assert!(round(0x1002) > 0);
    assert!(round(0x1004) > 0);
}

/// Thumb-2 coprocessor encodings that once wrote past the parse state.
/// `allocateOperand` was unbounded, so it ran `state[alloc++]` off the end of
/// `state` and wrote through the next node's `resolve` vector, taking the
/// process down.  The bound it carries now turns an overrun into a
/// `SleighError`, so the property is that the lift COMPLETES: p-code out and a
/// decode error are both acceptable, a crash is not.
///
/// These bytes do not descend further than legal code, though the crash they
/// caused made it look that way.  Measured, `0xdeec3b8a` peaks at 98
/// `ConstructState`s and breadcrumb depth 30, against 104 and 35 for
/// `vpush {s0-s31}`, the 32-register architectural maximum.  Both simply
/// exceeded the 75 states `ParserContext::initialize` used to hand out, which
/// is why sizing the parse state for legal ARM code lets these decode too.
#[test]
fn thumb_coproc_bytes_that_once_overran_the_parse_state_do_not_crash() {
    let arch = SleighArch::arm_thumb();
    for word in [
        0xdeec_3b8au32,
        0xdcec_3b8a,
        0xd8ec_3b8a,
        0xdeec_3b0a,
        0x9eec_3b8a,
    ] {
        let mut bytes = word.to_be_bytes().to_vec();
        bytes.resize(0x40, 0x70);
        let reader = BufMemReader::new(bytes, 0x1000);
        let mut sleigh =
            Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh arm_thumb");
        // Returning at all is the assertion; a regression takes the process
        // down here rather than failing.
        drop(sleigh.lift_one(0x1000));
    }
}
