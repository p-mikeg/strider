//! `DecodeEngine::build_cfg` driven by hand-crafted byte sequences, covering
//! the boundary shapes a real binary rarely reaches.

use rustc_hash::{FxHashMap, FxHashSet};

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{
    Cfg, CfgOptions, DecodeEngine, PcodeInsnAddr, RegionTerminator, ResolvedTargets,
};
use strider_target::SleighArch;

type TestReader = BufMemReader<Vec<u8>>;

fn make_sleigh_x86_64(bytes: Vec<u8>, base: u64) -> Sleigh<TestReader> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    arch.sleigh(reader).expect("create Sleigh x86_64")
}

fn build_from_bytes(bytes: Vec<u8>, start: u64) -> Cfg {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, start);
    DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(start.into(), &CfgOptions::default())
        .expect("build_cfg on synthetic bytes")
}

fn build_from_bytes_opts(bytes: Vec<u8>, start: u64, opts: &CfgOptions) -> Cfg {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, start);
    DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(start.into(), opts)
        .expect("build_cfg on synthetic bytes")
}

/// A call to a `no_return_targets` address ends its region even MID-function,
/// where the return address is in-bounds and the structural function-end
/// fallback cannot fire.  Without the mark the same call falls through to the
/// trailing `ret`.
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

    // Marked no-return: the region now ends AT the call.
    let no_return: FxHashSet<u64> = [0x2005].into_iter().collect();
    let opts = CfgOptions {
        no_return_targets: &no_return,
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn,
        "a call to a no-return target terminates the region"
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
fn split_both_halves_unconditional() {
    // The `jmp -4` targets 0x1002, mid-region.
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = build_from_bytes(bytes, 0x1000);

    let mut first_half = None;
    let mut second_half = None;
    for r in cfg.region_graph().node_weights() {
        if r.start_addr.machine() == 0x1000 {
            first_half = Some(r);
        } else if r.start_addr.machine() == 0x1002 {
            second_half = Some(r);
        }
    }
    let first_half = first_half.expect("first half (0x1000) region");
    let second_half = second_half.expect("second half (0x1002) region");

    assert_eq!(first_half.terminator, RegionTerminator::Unconditional);
    assert_eq!(second_half.terminator, RegionTerminator::Unconditional);
}

#[test]
fn size_bound_forces_forward_jump_to_be_tail_call() {
    // With a size bound of 0x10 the target 0x1012 is past the bound.
    let bytes = vec![0xeb, 0x10];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x10),
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
fn forward_jump_landing_exactly_at_size_bound_is_tail_call() {
    // Target 0x1010 lands EXACTLY on `start + size`.  The window is
    // half-open, so that is out of bounds and must be a tail call.  A `ret`
    // sits at 0x1010 so the bytes would decode if the edge were followed.
    let mut bytes = vec![0xeb, 0x0e]; // jmp +0x0e (next insn 0x1002 + 0x0e = 0x1010)
    bytes.extend(std::iter::repeat_n(0x90u8, 14)); // nop filler to 0x1010
    bytes.push(0xc3); // 0x1010: ret
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x10),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
    assert_eq!(cfg.region_graph().node_count(), 1);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::TailCall {
            target: 0x1010.into()
        },
        "addr == start + size must already be out-of-range (half-open bound)"
    );
}

#[test]
fn forward_jump_landing_just_inside_size_bound_is_followed() {
    // Companion probe one byte lower, strictly inside: the edge must be
    // followed and the target region decoded.
    let mut bytes = vec![0xeb, 0x0d]; // jmp +0x0d -> 0x1002 + 0x0d = 0x100f
    bytes.extend(std::iter::repeat_n(0x90u8, 13)); // nop filler to 0x100f
    bytes.push(0xc3); // 0x100f: ret
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x10),
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
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::Open {
            code_before_start: true,
        },
        ..CfgOptions::default()
    };
    let cfg = DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(0x1000u64.into(), &opts)
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
    // a size bound of 4.  The conditional survives, with the OOB arm lowered to a
    // stub wired as a regular CondBranch successor.
    let bytes = vec![0x31u8, 0xc0, 0x74, 0xfc];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(4),
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
    // The not-taken path falls past the end, as a sequential one does.
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1004, strider_cfg::Reason::PastFunctionEnd)]
    );
}

#[test]
fn cond_branch_with_oob_taken_target_keeps_cond_branch_with_tail_call_stub() {
    // Mirror of the OOB-fall-through test: here the TAKEN target 0x107e is
    // out of bounds and the fall-through 0x1004 is in range.
    let bytes = vec![0x31u8, 0xc0, 0x74, 0x7a, 0xc3];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x10),
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
        .find(|r| r.start_addr.machine() == 0x1004)
        .expect("fall-through region at 0x1004 must be decoded");
    assert_eq!(fallthrough.terminator, RegionTerminator::Return);
    // A branch out of range is a tail call, not a fall-through.
    assert_eq!(incomplete_of(&cfg), []);
}

#[test]
fn cond_branch_with_both_targets_oob_keeps_cond_branch_with_two_stubs() {
    // With a size bound of 2 both arms are out of bounds.  The conditional
    // survives with both lowered to stubs; collapsing to a single TailCall
    // would silently drop the fall-through arm.
    let bytes = vec![0x74u8, 0x7e];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(2),
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
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1002, strider_cfg::Reason::PastFunctionEnd)]
    );
}

#[test]
fn cond_branches_to_same_oob_target_share_one_stub() {
    // Two conditional branches to the SAME OOB address 0x1080.  Its stub must
    // be created once and shared: regions are keyed by start address, so a
    // second stub would collide.
    let bytes = vec![0x74u8, 0x7e, 0x75, 0x7c, 0xc3];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x10),
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

/// Every entry of `cfg.incomplete()`, as `(machine address, reason)`.
fn incomplete_of(cfg: &Cfg) -> Vec<(u64, strider_cfg::Reason)> {
    cfg.incomplete()
        .iter()
        .map(|(addr, reason)| (addr.machine(), reason))
        .collect()
}

/// The start addresses of the regions the region at `from` edges to.
fn successor_starts(cfg: &Cfg, from: u64) -> Vec<u64> {
    let graph = cfg.region_graph();
    let id = cfg
        .region_ids()
        .find(|&id| graph[id].start_addr == PcodeInsnAddr::at_machine_start(from))
        .unwrap_or_else(|| panic!("no region at {from:#x}"));
    graph
        .neighbors(id)
        .map(|succ| graph[succ].start_addr.machine())
        .collect()
}

#[test]
fn fall_through_past_size_bound_tail_calls_the_end() {
    // 0x1000 `xor eax, eax`, then a `lock cmpxchg` (multi-pcode-op, with
    // intra-insn CONST branches) at the 2-byte bound, never decoded: the
    // fall-through is a tail call to 0x1002.
    let mut bytes = vec![0x31u8, 0xc0];
    bytes.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(2),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    assert_tail_call_stub_at(&cfg, 0x1002);
    assert_eq!(successor_starts(&cfg, 0x1000), [0x1002]);
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1002, strider_cfg::Reason::PastFunctionEnd)]
    );
}

#[test]
fn fall_through_past_a_straddled_bound_leaves_after_the_instruction() {
    // `mov eax, 5` starts inside a 3-byte bound and decodes whole, so the
    // fall-through leaves at 0x1005, not at the bound.
    let bytes = vec![0xB8, 0x05, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(3),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    assert_tail_call_stub_at(&cfg, 0x1005);
    assert_eq!(successor_starts(&cfg, 0x1000), [0x1005]);
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1005, strider_cfg::Reason::PastFunctionEnd)]
    );
}

/// Under an open bound only an address below the entry is out of range, and
/// a fall-through only moves up.
#[test]
fn an_open_bound_never_ends_past_the_function() {
    // xor eax, eax; nop; ret
    for code_before_start in [false, true] {
        let opts = CfgOptions {
            bound: strider_cfg::FunctionBound::Open { code_before_start },
            ..CfgOptions::default()
        };
        let cfg = build_from_bytes_opts(vec![0x31, 0xc0, 0x90, 0xc3], 0x1000, &opts);
        assert_eq!(
            incomplete_of(&cfg),
            [],
            "code_before_start {code_before_start}"
        );
    }
}

/// A call whose return address is past the end still ends its region
/// `NoReturn`, and the return address is reported: the callee returning would
/// fall out of the function. A listed no-return callee never returns, so its
/// call is not reported.
#[test]
fn a_call_returning_past_size_bound_is_reported() {
    let opts = |no_return| CfgOptions {
        bound: strider_cfg::FunctionBound::size(5),
        no_return_targets: no_return,
        ..CfgOptions::default()
    };
    let none = FxHashSet::default();
    // 0x1000 call 0x2005
    let call = vec![0xe8, 0x00, 0x10, 0x00, 0x00, 0xc3];
    let cfg = build_from_bytes_opts(call.clone(), 0x1000, &opts(&none));
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn
    );
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1005, strider_cfg::Reason::PastFunctionEnd)]
    );

    let exit: FxHashSet<u64> = [0x2005].into_iter().collect();
    let cfg = build_from_bytes_opts(call, 0x1000, &opts(&exit));
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn
    );
    assert_eq!(incomplete_of(&cfg), []);

    // 0x1000 call rax, under a 2-byte bound
    let indirect = CfgOptions {
        bound: strider_cfg::FunctionBound::size(2),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(vec![0xff, 0xd0, 0xc3], 0x1000, &indirect);
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::NoReturn
    );
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1002, strider_cfg::Reason::PastFunctionEnd)]
    );
}

/// `bytes` at `start` under `arch`, bounded to `size`.
fn build_sized(arch: SleighArch, bytes: Vec<u8>, start: u64, size: u64) -> Cfg {
    let reader = BufMemReader::new(bytes, start);
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(size),
        ..CfgOptions::default()
    };
    DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(start.into(), &opts)
        .expect("build_cfg")
}

/// The entries of a build of `bytes` at 0x1000 bounded to their own length.
fn skip_past_end(arch: SleighArch, bytes: Vec<u8>) -> Vec<(u64, strider_cfg::Reason)> {
    let size = bytes.len() as u64;
    incomplete_of(&build_sized(arch, bytes, 0x1000, size))
}

// A conditional instruction skips itself with `if (!cc) goto inst_next`, so its
// not-executed path is a branch to its own end. Past the size end that is a
// fall-through out of the function, reported where it lands.

#[test]
fn an_arm_conditional_return_last_reports_its_skip() {
    let le = |words: &[u32]| -> Vec<u8> { words.iter().flat_map(|w| w.to_le_bytes()).collect() };
    // cmp r0, #0; then the conditional return
    for (what, last) in [("popne {pc}", 0x18bd_8000u32), ("bxne lr", 0x112f_ff1e)] {
        assert_eq!(
            skip_past_end(SleighArch::arm(), le(&[0xe350_0000, last])),
            [(0x1008, strider_cfg::Reason::PastFunctionEnd)],
            "{what}"
        );
    }
}

#[test]
fn a_mips_branch_likely_last_reports_its_skip() {
    // beql a0, zero, 0x1000; nop: not taken, it skips its delay slot to 0x1008.
    let be = |words: &[u32]| -> Vec<u8> { words.iter().flat_map(|w| w.to_be_bytes()).collect() };
    assert_eq!(
        skip_past_end(SleighArch::mipsbe32(), be(&[0x5080_ffff, 0])),
        [(0x1008, strider_cfg::Reason::PastFunctionEnd)]
    );
}

#[test]
fn an_x86_rep_last_reports_its_skip() {
    // rep movsb
    assert_eq!(
        skip_past_end(SleighArch::x86_64(), vec![0xf3, 0xa4]),
        [(0x1002, strider_cfg::Reason::PastFunctionEnd)]
    );
}

#[test]
fn an_x86_conditional_branch_last_reports_its_not_taken_side() {
    // cmp eax, eax; jne 0x1000: the taken side stays in range, the
    // fall-through leaves the function.
    assert_eq!(
        skip_past_end(SleighArch::x86_64(), vec![0x39, 0xc0, 0x75, 0xfc]),
        [(0x1004, strider_cfg::Reason::PastFunctionEnd)]
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
fn zero_pcode_prefix_crossing_size_bound_tail_calls_the_end() {
    // Three nops, then the next function's `ret` at 0x1003. The nops append
    // no pcode op, so `insns` stays empty while decode walks past the 2-byte
    // bound; the fall-through must still stop at 0x1002 rather than absorb
    // the `ret`.
    let bytes = vec![0x90u8, 0x90, 0x90, 0xc3];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(2),
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

    assert_tail_call_stub_at(&cfg, 0x1002);
    assert_eq!(successor_starts(&cfg, 0x1001), [0x1002]);
    assert!(
        cfg.regions()
            .all(|r| r.terminator != RegionTerminator::Return),
        "the next function's ret was absorbed"
    );
    assert_eq!(
        incomplete_of(&cfg),
        [(0x1002, strider_cfg::Reason::PastFunctionEnd)]
    );
}

#[test]
fn size_bound_smaller_than_first_terminator_insn_still_builds_tail_call() {
    // A 2-byte `jmp +0x10` under a size bound of 1 starts in range but its
    // encoding crosses the bound.  Decoding is NOT length-bounded: the
    // instruction decodes fully and its OOB target is a tail call.  Contrast
    // the past-end cases above; the bound trips only on sequential
    // fall-through, never mid-instruction.
    let bytes = vec![0xebu8, 0x10]; // jmp +0x10 -> target 0x1012
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(1),
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
    assert_eq!(cfg.region_graph()[cfg.entry()].start_addr.machine(), 0x1000);
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
    DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(start.into(), opts)
        .expect("build_cfg on synthetic ppc words")
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
        .flat_map(|r| r.insns.iter().map(|i| i.addr.machine()))
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
        call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
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

/// `jmp rax` at 0x1000 followed by 16 `int3`.
fn jmp_rax_bytes() -> Vec<u8> {
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    bytes
}

fn build_unresolved_jmp_rax_cfg() -> Cfg {
    build_from_bytes(jmp_rax_bytes(), 0x1000)
}

/// Panics unless the CFG holds an `UnresolvedIndirectBranch` region.
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
fn with_known_targets_link_register_overrides_to_return() {
    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::LinkRegister);

    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    let cfg_v2 = build_from_bytes_opts(jmp_rax_bytes(), 0x1000, &opts);

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
fn known_multiple_with_out_of_range_target_defers_to_unresolved() {
    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        unresolved_addr,
        ResolvedTargets::table([0x1004, 0x9000].map(strider_cfg::ResolvedTarget::from)),
    );

    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x100),
        known_targets: &known,
        ..CfgOptions::default()
    };
    // Must succeed: a mixed Multiple defers via UnresolvedIndirectBranch.
    let cfg = build_from_bytes_opts(jmp_rax_bytes(), 0x1000, &opts);

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
    let oob_target = 0x9000u64;

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(unresolved_addr, ResolvedTargets::Single(oob_target.into()));

    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x100),
        known_targets: &known,
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(jmp_rax_bytes(), 0x1000, &opts);

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
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0x90u8, 32));
    bytes.push(0xc3);

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        unresolved_addr,
        ResolvedTargets::table([0x1004, 0x1008].map(strider_cfg::ResolvedTarget::from)),
    );

    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x100),
        known_targets: &known,
        ..CfgOptions::default()
    };
    let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);

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
        ResolvedTargets::table(vec![0x1002.into(), 0x1005.into()]),
    );
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let cfg = DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(base.into(), &opts)
        .expect("build with an over-read switch arm");

    let starts: Vec<u64> = cfg.regions().map(|r| r.start_addr.machine()).collect();
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
        ResolvedTargets::table(vec![0x1005.into(), 0x2000.into()]),
    );
    known.insert(
        PcodeInsnAddr::at_machine_start(0x2000),
        ResolvedTargets::table(vec![0x1002.into(), 0x1008.into()]),
    );
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let cfg = DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(base.into(), &opts)
        .expect("build with an over-read switch arm shadowed by an overlapping region");

    let starts: Vec<u64> = cfg.regions().map(|r| r.start_addr.machine()).collect();
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
        ResolvedTargets::table(vec![0x1002.into(), 0x1005.into()]),
    );
    let opts = CfgOptions {
        known_targets: &known,
        ..CfgOptions::default()
    };
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let cfg = DecodeEngine::new(arch, reader)
        .expect("decode engine")
        .build_cfg(base.into(), &opts)
        .expect("build with an over-read switch arm inside a zero-pcode-op instruction");

    let starts: Vec<u64> = cfg.regions().map(|r| r.start_addr.machine()).collect();
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
        if region.start_addr.machine() == 0x1002 {
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
/// them. The seated target must still decode rather than have its context
/// rejected.
///
/// `bx r0 ; itte eq ; movs r0,#0 x3 ; bx lr`, seating 0x1002 in round one and
/// 0x1004 in round two, which is the orchestrator's re-lift shape.
#[test]
fn re_imposing_a_context_over_an_it_block_builds() {
    let arch = SleighArch::arm_thumb();
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x0c].copy_from_slice(&[
        0x00, 0x47, 0x06, 0xbf, 0x00, 0x20, 0x00, 0x20, 0x00, 0x20, 0x70, 0x47,
    ]);
    let reader = BufMemReader::new(bytes, 0x1000);
    let mut engine = DecodeEngine::new(arch, reader).expect("decode engine");

    // Entered with the Thumb bit, as production enters a Thumb function; the
    // engine pins the entry context each round.
    let mut round = |target: u64| {
        let mut known = FxHashMap::default();
        known.insert(
            PcodeInsnAddr::at_machine_start(0x1000),
            ResolvedTargets::table(vec![target.into()]),
        );
        let opts = CfgOptions {
            known_targets: &known,
            ..CfgOptions::default()
        };
        let cfg = engine
            .build_cfg(0x1001u64.into(), &opts)
            .expect("re-imposing a context over an IT block must build");
        // A rejected pin does not fail the build: it reports the arm.
        assert_eq!(cfg.undecodable_seeded_targets(), [], "seating {target:#x}");
        assert!(
            cfg.regions().any(|r| r.start_addr.machine() == target),
            "a region starts at the seated {target:#x}",
        );
    };

    round(0x1002);
    round(0x1004);
}

/// Thumb-2 coprocessor encodings needing more parse states than a small
/// `ParserContext` holds (`0xdeec3b8a` peaks at 98 `ConstructState`s; legal
/// `vpush {s0-s31}` needs 104). The lift must complete: p-code or a decode
/// error, never a crash.
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
        let mut sleigh = arch.sleigh(reader).expect("create Sleigh arm_thumb");
        // Returning at all is the assertion; a regression takes the process
        // down here rather than failing.
        drop(sleigh.lift_one(0x1000));
    }
}

/// A size bound cutting through the last instruction leaves the fall-through
/// region's span overhanging the bound, and an OOB taken target landing in that
/// overhang is owned by two regions: the stub that starts at it, and the
/// fall-through region whose last instruction covers it.  Polarity must name
/// the stub, so the two runs below differ only in the last instruction's byte
/// length and must report the SAME arms.
#[test]
fn oob_taken_target_inside_the_fallthrough_overhang_keeps_its_polarity() {
    // 0x1000: je 0x1011      (0f 84 0b 00 00 00), out of bounds at 0x1010
    // 0x1006: xor rax,rax x3 (48 31 c0)
    // 0x100f: the trailing instruction, 3 bytes or 1
    let prefix = [
        0x0fu8, 0x84, 0x0b, 0x00, 0x00, 0x00, 0x48, 0x31, 0xc0, 0x48, 0x31, 0xc0, 0x48, 0x31, 0xc0,
    ];
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(0x10),
        ..CfgOptions::default()
    };
    // `ret 0x12` spans 0x100f..0x1012 and overhangs; `ret` spans one byte.
    for (tail, what) in [
        ([0xc2u8, 0x12, 0x00], "overhanging last instruction"),
        ([0xc3u8, 0x90, 0x90], "last instruction inside the bound"),
    ] {
        let bytes: Vec<u8> = prefix.iter().chain(tail.iter()).copied().collect();
        let cfg = build_from_bytes_opts(bytes, 0x1000, &opts);
        let succ = cfg.region_if(cfg.entry()).expect("entry is a CondBranch");
        let start_of = |id: Option<petgraph::graph::NodeIndex>| {
            cfg.region_graph()[id.expect("both arms are wired")]
                .start_addr
                .machine()
        };
        assert_eq!(
            start_of(succ.if_true_region),
            0x1011,
            "{what}: the taken arm is the stub at the branch target"
        );
        assert_eq!(
            start_of(succ.if_false_region),
            0x1006,
            "{what}: the fall-through arm is the region after the branch"
        );
    }
}

/// A region whose sequential decode walks off the end of the mapped image
/// seals at its last decoded instruction and seats the same stub a direct
/// branch to those bytes gets, rather than failing the whole function.
#[test]
fn fallthrough_off_the_end_of_the_mapped_image_seats_a_stub() {
    // 0x1000: xor rax,rax (48 31 c0); the fall-through 0x1003 has no bytes.
    let cfg = build_from_bytes(vec![0x48u8, 0x31, 0xc0], 0x1000);

    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UnmappedTarget)
            .collect::<Vec<_>>(),
        &[PcodeInsnAddr::at_machine_start(0x1003)],
        "the unmapped fall-through is reported, not silently dropped"
    );
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::Unconditional,
        "the decoded instructions are kept as a sealed region"
    );
    assert_tail_call_stub_at(&cfg, 0x1003);
    assert_eq!(cfg.region_graph().edge_count(), 1);
}

/// A direct branch and a fall-through reaching one unmapped address report it
/// once: the direct branch seats the stub from `Builder::build`'s error path,
/// and the fall-through fails `lift_code` before it checks the region map.
#[test]
fn one_unmapped_address_two_edges_reach_is_reported_once() {
    // 0x1000 je 0x1006 (74 04) ; 0x1002 jmp 0x1008 (eb 04) ; 0x1004 ret ;
    // 0x1005 ret ; 0x1006 xor eax,eax (31 c0), falling through to 0x1008,
    // which the image has no bytes for.
    let cfg = build_from_bytes(vec![0x74, 0x04, 0xeb, 0x04, 0xc3, 0xc3, 0x31, 0xc0], 0x1000);
    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UnmappedTarget)
            .collect::<Vec<_>>(),
        &[PcodeInsnAddr::at_machine_start(0x1008)],
    );
}

/// The window ending mid-instruction is the same verdict as it ending on the
/// boundary: Sleigh raises `PartiallyInitializedInsn` rather than
/// `DataUnavailErr`, and the region still seals on a stub.
#[test]
fn a_fallthrough_straddling_the_end_of_the_mapped_image_seats_a_stub() {
    // 0x1000: xor rax,rax (48 31 c0); 0x1003 holds one byte of a three-byte
    // instruction, so its decode reads past the end of the image.
    let cfg = build_from_bytes(vec![0x48u8, 0x31, 0xc0, 0x48], 0x1000);

    assert_eq!(
        cfg.incomplete()
            .of(strider_cfg::Reason::UnmappedTarget)
            .collect::<Vec<_>>(),
        &[PcodeInsnAddr::at_machine_start(0x1003)],
        "the straddling fall-through is reported, not silently dropped"
    );
    assert_eq!(
        cfg.region_graph()[cfg.entry()].terminator,
        RegionTerminator::Unconditional,
        "the decoded instructions are kept as a sealed region"
    );
    assert_tail_call_stub_at(&cfg, 0x1003);
    assert_eq!(cfg.region_graph().edge_count(), 1);
}

/// A finished region hands on an exactly-sized instruction vector; the push
/// doubling would otherwise carry its slack for the whole lift.
#[test]
fn a_finished_region_keeps_no_spare_instruction_capacity() {
    let mut bytes = Vec::new();
    for _ in 0..7 {
        bytes.extend_from_slice(&[0x48, 0xff, 0xc0]); // inc rax
    }
    bytes.push(0xc3); // ret
    let cfg = build_from_bytes(bytes, 0x1000);
    let mut filled = 0usize;
    for region in cfg.regions() {
        assert_eq!(
            region.insns.capacity(),
            region.insns.len(),
            "region at {:?} carries spare instruction capacity",
            region.start_addr
        );
        filled += usize::from(!region.insns.is_empty());
    }
    assert!(filled > 0, "expected a region holding instructions");
}

/// A GCC hot/cold split: `foo` at 0x1000 `js`es down to its cold fragment
/// at 0x0ff0, which jumps back into the body at 0x1005.
///
/// ```text
/// 0x0ff0: add eax, 1        83 c0 01
/// 0x0ff3: jmp 0x1005        eb 10
/// 0x0ff5: nop x11
/// 0x1000: test edi, edi     85 ff
/// 0x1002: js 0x0ff0         78 ec
/// 0x1004: nop               90
/// 0x1005: ret               c3
/// ```
fn hot_cold_split(fragment_ranges: Vec<(u64, u64)>) -> Cfg {
    let mut bytes = vec![0x83, 0xc0, 0x01, 0xeb, 0x10];
    bytes.extend(std::iter::repeat_n(0x90u8, 11));
    bytes.extend([0x85, 0xff, 0x78, 0xec, 0x90, 0xc3]);
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(6),
        fragment_ranges: strider_cfg::DataRanges::try_new("fragment_ranges", fragment_ranges)
            .unwrap(),
        ..CfgOptions::default()
    };
    DecodeEngine::new(SleighArch::x86_64(), BufMemReader::new(bytes, 0x0ff0))
        .expect("decode engine")
        .build_cfg(0x1000u64.into(), &opts)
        .expect("build_cfg")
}

#[test]
fn a_branch_into_an_unlisted_fragment_is_a_tail_call() {
    let cfg = hot_cold_split(Vec::new());
    assert_tail_call_stub_at(&cfg, 0x0ff0);
    assert!(incomplete_of(&cfg).is_empty());
}

#[test]
fn a_fragment_range_is_decoded_and_its_jump_back_is_an_edge() {
    let cfg = hot_cold_split(vec![(0x0ff0, 0x0ff5)]);
    assert_eq!(successor_starts(&cfg, 0x0ff0), vec![0x1005]);
    let mut from_entry = successor_starts(&cfg, 0x1000);
    from_entry.sort_unstable();
    assert_eq!(from_entry, vec![0x0ff0, 0x1004]);
    assert!(
        !cfg.regions()
            .any(|r| matches!(r.terminator, RegionTerminator::TailCall { .. })),
        "nothing leaves the function"
    );
    assert!(incomplete_of(&cfg).is_empty());
}

#[test]
fn a_fall_through_past_a_fragment_end_is_past_function_end() {
    let cfg = hot_cold_split(vec![(0x0ff0, 0x0ff3)]);
    assert_tail_call_stub_at(&cfg, 0x0ff3);
    assert_eq!(
        incomplete_of(&cfg),
        vec![(0x0ff3, strider_cfg::Reason::PastFunctionEnd)]
    );
}

/// A fragment above the entry ends the function under every bound: the
/// `ret` at 0x1013 after the fragment `[0x1010, 0x1013)` is not decoded.
///
/// ```text
/// 0x1000: test edi, edi     85 ff
/// 0x1002: js 0x1010         78 0c
/// 0x1004: ret               c3
/// 0x1005: nop x11
/// 0x1010: add eax, 1        83 c0 01
/// 0x1013: ret               c3
/// ```
#[test]
fn a_fall_through_past_a_fragment_above_the_entry_is_past_function_end() {
    let mut bytes = vec![0x85, 0xff, 0x78, 0x0c, 0xc3];
    bytes.extend(std::iter::repeat_n(0x90u8, 11));
    bytes.extend([0x83, 0xc0, 0x01, 0xc3]);
    for bound in [
        strider_cfg::FunctionBound::size(5),
        strider_cfg::FunctionBound::Open {
            code_before_start: false,
        },
        strider_cfg::FunctionBound::Open {
            code_before_start: true,
        },
    ] {
        let opts = CfgOptions {
            bound,
            fragment_ranges: strider_cfg::DataRanges::try_new(
                "fragment_ranges",
                vec![(0x1010, 0x1013)],
            )
            .unwrap(),
            ..CfgOptions::default()
        };
        let cfg = DecodeEngine::new(
            SleighArch::x86_64(),
            BufMemReader::new(bytes.clone(), 0x1000),
        )
        .expect("decode engine")
        .build_cfg(0x1000u64.into(), &opts)
        .expect("build_cfg");
        assert_tail_call_stub_at(&cfg, 0x1013);
        assert_eq!(
            incomplete_of(&cfg),
            vec![(0x1013, strider_cfg::Reason::PastFunctionEnd)],
            "{bound:?}"
        );
    }
}

/// A branch into the middle of an instruction enters at that instruction,
/// whether or not another edge also splits there.
#[test]
fn an_interior_target_enters_at_its_containing_instruction() {
    // 0x1000 xor eax, eax
    // 0x1002 mov eax, 1        (5 bytes)
    // 0x1007 je 0x1003         interior to the mov
    // 0x1009 ret, or jne 0x1002; ret
    let alone = [0x31, 0xc0, 0xb8, 1, 0, 0, 0, 0x74, 0xfa, 0xc3];
    let split_too = [0x31, 0xc0, 0xb8, 1, 0, 0, 0, 0x74, 0xfa, 0x75, 0xf7, 0xc3];
    for bytes in [&alone[..], &split_too[..]] {
        let cfg = build_from_bytes(bytes.to_vec(), 0x1000);
        let graph = cfg.region_graph();
        let je = graph
            .node_indices()
            .find(|&n| {
                graph[n]
                    .insns
                    .back()
                    .is_some_and(|i| i.addr.machine() == 0x1007)
            })
            .expect("the region ending at the je");
        let taken = cfg.region_if(je).expect("CondBranch").if_true_region;
        assert_eq!(
            taken.map(|t| graph[t].start_addr.machine()),
            Some(0x1002),
            "{bytes:x?}: the taken edge enters at the mov, not before it",
        );
        let interior: Vec<_> = cfg
            .incomplete()
            .of(strider_cfg::Reason::InteriorTarget)
            .map(|a| a.machine())
            .collect();
        assert_eq!(interior, [0x1003], "{bytes:x?}");
    }
}
