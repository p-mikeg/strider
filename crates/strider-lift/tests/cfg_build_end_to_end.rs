#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end tests for `Builder::build` driven by small hand-crafted
//! x86-64 byte sequences.  Covers scenarios that real binaries don't
//! exercise cleanly: region splitting by back-jump, `fn_max_size`
//! tail-call classification, `allow_code_before_start_addr`,
//! `CondBranch`-with-OOB-successor folds, multi-pcode insn past
//! `fn_max_size`, and Sleigh handle re-use across successive builds.
//!
//! Ported from pre-rewrite `crates/cfg/tests/{build_end_to_end,
//! sleigh_reuse,region_terminates_on_noreturn_callother}.rs`.

use rustc_hash::FxHashMap;

use rsleigh::mem_readers::BufMemReader;
use rsleigh::Sleigh;
use strider_lift::cfg::{
    Builder, Cfg, OptionsBuilder, PcodeInsnAddr, RegionTerminator, ResolvedTargets,
};
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
    Builder::for_arch(&arch, &mut sleigh, start, OptionsBuilder::new().build())
        .build()
        .expect("Builder::build on synthetic bytes")
}

fn build_from_bytes_opts(
    bytes: Vec<u8>,
    start: u64,
    opts: strider_lift::cfg::OptionsBuilder,
) -> Cfg {
    let arch = SleighArch::x86_64();
    let mut sleigh = make_sleigh_x86_64(bytes, start);
    Builder::for_arch(&arch, &mut sleigh, start, opts.build())
        .build()
        .expect("Builder::build on synthetic bytes")
}

// ── single-region / smoke tests ──────────────────────────────────────────

#[test]
fn single_ret_produces_one_region_without_tail_call_flag() {
    let cfg = build_from_bytes(vec![0xc3], 0x1000);
    assert_eq!(cfg.region_graph.node_count(), 1);
    assert_eq!(cfg.region_graph[cfg.entry].terminator, RegionTerminator::Return);
}

// ── region-split tests ───────────────────────────────────────────────────

#[test]
fn back_jump_splits_region() {
    // xor eax,eax (2 bytes); xor eax,eax (2 bytes); jmp -4 (2 bytes, back
    // to 0x1002).  Target 0x1002 is mid-region, triggering split_region.
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = build_from_bytes(bytes, 0x1000);
    assert!(
        cfg.region_graph.node_count() >= 2,
        "expected at least 2 regions after back-jump split; got {}",
        cfg.region_graph.node_count()
    );
    // The back-jump produces an edge from the branch region to the split
    // second half (edges are unweighted; the `Branch` terminator classifies
    // it).
    assert!(
        cfg.region_graph.edge_count() >= 1,
        "expected at least one edge from the back-jump"
    );
}

#[test]
fn split_first_half_becomes_fallthrough_second_half_branch() {
    // Same back-jump fixture, with stricter assertions on per-half
    // terminators.  Ported from pre-rewrite region_terminator.rs.
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = build_from_bytes(bytes, 0x1000);

    let mut first_half = None;
    let mut second_half = None;
    for r in cfg.region_graph.node_weights() {
        if r.start_addr.machine_addr.addr == 0x1000 {
            first_half = Some(r);
        } else if r.start_addr.machine_addr.addr == 0x1002 {
            second_half = Some(r);
        }
    }
    let first_half = first_half.expect("first half (0x1000) region");
    let second_half = second_half.expect("second half (0x1002) region");

    assert_eq!(first_half.terminator, RegionTerminator::Fallthrough);
    assert_eq!(second_half.terminator, RegionTerminator::Branch);
}

// ── fn_max_size / tail-call classification ───────────────────────────────

#[test]
fn fn_max_size_forces_forward_jump_to_be_tail_call() {
    // jmp +0x10 at 0x1000.  With fn_max_size=0x10, target 0x1012 >=
    // 0x1000+0x10 -> tail call.
    let bytes = vec![0xeb, 0x10];
    let opts = OptionsBuilder::new().set_function_max_size(0x10);
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);
    assert_eq!(cfg.region_graph.node_count(), 1);
    assert_eq!(
        cfg.region_graph[cfg.entry].terminator,
        RegionTerminator::TailCall { target: 0x1012 }
    );
}

#[test]
fn allow_code_before_start_addr_negates_below_start_tail_call() {
    // `ret` below the function start; jmp -16 from 0x1000 -> 0x0ff2.
    let mut bytes = vec![0u8; 0x14];
    bytes[0x02] = 0xc3; // 0x0ff2: ret
    bytes[0x10] = 0xeb; // 0x1000: jmp
    bytes[0x11] = 0xf0; // rel8 = -16 -> 0x0ff2

    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, 0x0ff0);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = OptionsBuilder::new().allow_code_before_start_addr().build();
    let cfg = Builder::for_arch(&arch, &mut sleigh, 0x1000, opts).build().unwrap();

    assert_ne!(
        cfg.region_graph[cfg.entry].terminator,
        RegionTerminator::TailCall { target: 0x0ff2 },
        "entry region must NOT be a TailCall when allow_code_before_start_addr is set"
    );
    assert!(
        cfg.region_graph.edge_count() >= 1,
        "expected at least one edge since the below-start target is followed"
    );
}

#[test]
fn cond_branch_with_oob_fallthrough_collapses_to_branch_in_range() {
    // xor eax,eax (2 bytes, sets ZF); je -4 (2 bytes, taken=0x1000 in-range,
    // fall-through=0x1004 OOB at fn_max_size=4).
    let mut bytes = vec![0x31u8, 0xc0, 0x74, 0xfc];
    bytes.extend(std::iter::repeat_n(0x90u8, 64));
    let opts = OptionsBuilder::new().set_function_max_size(4);
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);

    assert!(
        !matches!(cfg.region_graph[cfg.entry].terminator, RegionTerminator::CondBranch { .. }),
        "entry region must not retain CondBranch when one successor is OOB"
    );
}

#[test]
fn cond_branch_with_both_targets_oob_collapses_to_tail_call() {
    // je +0x7e (2 bytes) at 0x1000 with fn_max_size=2 -> taken 0x1080 OOB,
    // fall-through 0x1002 also OOB.
    let mut bytes = vec![0x74u8, 0x7e];
    bytes.extend(std::iter::repeat_n(0x90u8, 256));
    let opts = OptionsBuilder::new().set_function_max_size(2);
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);

    assert!(
        matches!(cfg.region_graph[cfg.entry].terminator, RegionTerminator::TailCall { .. }),
        "entry region must collapse to TailCall when both CondBranch successors are OOB; got {:?}",
        cfg.region_graph[cfg.entry].terminator
    );
}

#[test]
fn fall_through_past_fn_max_size_terminates_as_tail_call() {
    // xor eax,eax (2 bytes); lock cmpxchg %r14, 0x58(%rbx) (6 bytes,
    // multi-pcode-op with intra-insn CONST branches).
    let mut bytes = vec![0x31u8, 0xc0];
    bytes.extend_from_slice(&[0xF0, 0x4C, 0x0F, 0xB1, 0x73, 0x58]);
    bytes.extend(std::iter::repeat_n(0x90u8, 16));
    let opts = OptionsBuilder::new().set_function_max_size(2);
    let cfg = build_from_bytes_opts(bytes, 0x1000, opts);

    assert_eq!(
        cfg.region_graph[cfg.entry].terminator,
        RegionTerminator::TailCall { target: 0x1002 }
    );
}

// ── CallOther NoReturn termination (x86 ud2) ────────────────────────────

#[test]
fn ud2_region_finishes_as_noreturn() {
    // x86_64 ud2 = 0x0F 0x0B.  Sleigh emits a CallOther [user_op =
    // invalidInstructionException] followed by BranchIndirect; the
    // region builder must terminate at the CallOther (classify =
    // NoReturn) before the trailing BranchIndirect routes.
    let bytes = vec![0x0fu8, 0x0b];
    let cfg = build_from_bytes(bytes, 0x1000);

    let any_noreturn = cfg
        .region_graph
        .node_weights()
        .any(|r| matches!(r.terminator, RegionTerminator::NoReturn));
    assert!(
        any_noreturn,
        "expected at least one NoReturn region; got terminators: {:?}",
        cfg.region_graph
            .node_weights()
            .map(|r| &r.terminator)
            .collect::<Vec<_>>()
    );
    let any_unresolved = cfg.region_graph.node_weights().any(|r| {
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

// ── Sleigh handle re-use across builds ───────────────────────────────────

fn build_one(mut sleigh: Sleigh<TestReader>, start: u64) -> (Cfg, Sleigh<TestReader>) {
    let arch = SleighArch::x86_64();
    let cfg = Builder::for_arch(&arch, &mut sleigh, start, OptionsBuilder::new().build())
        .build()
        .expect("Builder::build");
    (cfg, sleigh)
}

#[test]
fn cfg_build_returns_sleigh_for_reuse() {
    let bytes = vec![0xc3u8];
    let sleigh = make_sleigh_x86_64(bytes, 0x1000);

    // `build` hands the Sleigh back so a subsequent rebuild can reuse it
    // without re-loading the SLA spec (the Cfg itself never owns it).
    let (cfg1, sleigh) = build_one(sleigh, 0x1000);
    assert!(cfg1.region_graph.node_count() >= 1);

    let (cfg2, _sleigh) = build_one(sleigh, 0x1000);
    assert!(cfg2.region_graph.node_count() >= 1);
}

#[test]
fn sleigh_can_be_used_for_multiple_cfg_builds() {
    let bytes_a = vec![0xc3u8];
    let sleigh = make_sleigh_x86_64(bytes_a, 0x1000);
    let (cfg1, sleigh) = build_one(sleigh, 0x1000);
    let count1 = cfg1.region_graph.node_count();
    let (cfg2, sleigh) = build_one(sleigh, 0x1000);
    let count2 = cfg2.region_graph.node_count();
    let (cfg3, _final_sleigh) = build_one(sleigh, 0x1000);
    assert!(count1 >= 1);
    assert!(count2 >= 1);
    assert!(cfg3.region_graph.node_count() >= 1);
    let _ = cfg3.entry;
}

// ── known_targets feedback path ─────────────────────────────────────────

fn build_unresolved_jmp_rax_cfg() -> Cfg {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    Builder::for_arch(&arch, &mut sleigh, base, OptionsBuilder::new().build())
        .build()
        .expect("build")
}

fn locate_unresolved_addr(cfg: &Cfg) -> PcodeInsnAddr {
    for region_id in cfg.region_ids() {
        let region = cfg.region_graph.node_weight(region_id).expect("region");
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

    let cfg_v2 = Builder::for_arch(&arch, &mut sleigh, base, OptionsBuilder::new().build())
        .with_known_targets(known)
        .build()
        .expect("build with known_targets");

    let mut had_return = false;
    for region in cfg_v2.regions() {
        if matches!(region.terminator, RegionTerminator::Return) {
            had_return = true;
        }
        assert!(
            !matches!(region.terminator, RegionTerminator::UnresolvedIndirectBranch { .. }),
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

    let cfg = Builder::for_arch(&arch, &mut sleigh, base, OptionsBuilder::new().build())
        .with_known_targets(FxHashMap::default())
        .build()
        .expect("build with empty known_targets");

    let had_unresolved = cfg
        .regions()
        .any(|r| matches!(r.terminator, RegionTerminator::UnresolvedIndirectBranch { .. }));
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
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        unresolved_addr,
        ResolvedTargets::Multiple(vec![0x1004, 0x9000]),
    );

    let cfg = Builder::for_arch(&arch, &mut sleigh, base, opts)
        .with_known_targets(known)
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

#[test]
fn known_multiple_in_range_targets_produces_switch() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0x90u8, 32));
    bytes.push(0xc3);
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        unresolved_addr,
        ResolvedTargets::Multiple(vec![0x1004, 0x1008]),
    );

    let cfg = Builder::for_arch(&arch, &mut sleigh, base, opts)
        .with_known_targets(known)
        .build()
        .expect("build with in-range Multiple must succeed");

    let had_switch = cfg
        .regions()
        .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. }));
    assert!(had_switch, "in-range Multiple must produce a Switch terminator");
}
