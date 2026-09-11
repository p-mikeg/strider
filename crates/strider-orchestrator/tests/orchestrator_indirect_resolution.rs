mod common;

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
use strider_target::{CallingConvention, SleighArch};

fn make_sleigh_value(bytes: Vec<u8>, base: u64) -> Sleigh<BufMemReader<Vec<u8>>> {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh")
}

/// x86_64 SystemV, default options.
fn run_at(bytes: Vec<u8>, base: u64) -> anyhow::Result<strider_orchestrator::AnalyzeResult> {
    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes, base);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None)?;
    strider.analyze(
        base,
        &cc,
        &LiftOptions::default(),
        &OptOptions::default(),
        None,
    )
}

#[test]
fn outer_loop_zero_iter_when_no_branch_indirect_returns_ir() {
    // No BranchIndirect (just `ret`), so the resolution loop converges on
    // its first pass without a re-lift.
    let bytes = vec![0xc3u8]; // ret
    let function = run_at(bytes, 0x1000).expect("orchestrator").function;
    let mut had_return = false;
    for nid in function.walk() {
        if matches!(function.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(had_return);
}

#[test]
fn outer_loop_unresolved_branch_is_reported_not_errored() {
    // `jmp rax`: rax is a function-entry value (no constant write), and
    // x86_64 has no link register, so the resolver can't classify it. The
    // orchestrator must reach a fixed point and return the branch listed in
    // `unresolved_indirect_branches`.
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax, sole machine insn at 0x1000
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let result =
        run_at(bytes, 0x1000).expect("analyze must return Ok even when a branch is unresolvable");
    // `jmp rax` lifts to a single BRANCHIND pcode op, hence insn_index 0.
    assert_eq!(
        result.unresolved_indirect_branches,
        vec![strider_cfg::PcodeInsnAddr {
            machine_addr: strider_cfg::MachineInsnAddr::from(0x1000u64),
            insn_index: 0,
        }],
        "unresolved list must contain exactly the jmp-rax site"
    );
    let placeholder_count = result
        .function
        .walk()
        .filter(|&n| {
            matches!(
                result.function.node_kind(n),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count();
    assert_eq!(
        placeholder_count, 1,
        "exactly one unresolved IndirectBranch placeholder must remain in the returned IR"
    );
}

#[test]
fn analyze_seats_pre_seeded_known_targets_from_lift_options() {
    // The caller's `cfg.known_targets` seeds the resolution loop, which then
    // grows it. `apply_resolutions` unions, so the FOLD only ever adds edges
    // to what the classifier proved -- seating one can still cost the site its
    // real arms (`known_targets_union.rs`). Here `jmp rax` is unresolvable, so
    // the seed is the only answer the site has.
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax at 0x1000
    bytes.extend(std::iter::repeat_n(0xccu8, 16));

    let site = strider_cfg::PcodeInsnAddr {
        machine_addr: strider_cfg::MachineInsnAddr::from(0x1000u64),
        insn_index: 0,
    };
    let mut known = rustc_hash::FxHashMap::default();
    // 0x1004 is in-range padding: a valid (decodable) seed target.
    known.insert(site, strider_cfg::ResolvedTargets::Single(0x1004.into()));
    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..LiftOptions::default()
    };

    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes, 0x1000);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(0x1000, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("analyze");
    assert_eq!(
        result.unresolved_indirect_branches,
        vec![],
        "the seeded site is answered, so nothing is left unresolved"
    );
    let placeholder_survives = result.function.walk().any(|n| {
        matches!(
            result.function.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        !placeholder_survives,
        "the caller's answer seats the dispatch, replacing the placeholder"
    );
    // 0x1004 is in-function, so the seat is a one-arm `Switch`; naming the
    // address is what stops a wrong seat passing.
    assert_eq!(
        result
            .cfg
            .regions()
            .filter_map(|region| match &region.terminator {
                strider_cfg::RegionTerminator::Switch { targets, .. } =>
                    Some(targets.iter().map(|t| t.addr).collect::<Vec<_>>()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![vec![0x1004u64]],
        "the caller's answer must be the seated successor",
    );
}

#[test]
fn outer_loop_resolves_via_stack_load_forward_for_x86_64_push_pop() {
    // `push imm32; pop rax; jmp rax` is structurally a tail call. After
    // StackOffsetDetect + LoadForward the placeholder's dispatch input
    // folds to IntConst(K); IndirectBranchClassify reads that live input
    // and classifies Single(K). K lies outside the function range (below
    // start_addr), so the orchestrator seats it as a tail call and the
    // rebuild lowers it to `Call(K) + Return`.
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));

    let function = run_at(bytes, 0x1000)
        .expect("push/pop/jmp of a constant must resolve to a tail call")
        .function;
    let placeholder_survives = function.walk().any(|n| {
        matches!(
            function.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        !placeholder_survives,
        "expected the IndirectBranch placeholder to be resolved into a tail call, \
         but one survived in the final graph"
    );
    // The placeholder going away is not the claim: a pass that merely deleted
    // it, or seated the wrong K, would pass the check above.
    assert!(
        call_to(&function, k).is_some(),
        "the tail call must target {k:#x}; found {:#x?}",
        call_targets(&function),
    );
}

/// The `Call` whose target input is `IntConst(target)`, i.e. the seated tail
/// call.
fn call_to(function: &strider_ir::Function, target: u64) -> Option<strider_ir::node::NodeId> {
    function.walk().find(|&nid| {
        matches!(function.node_kind(nid), strider_ir::node::NodeKind::Call)
            && function
                .node_inputs(nid)
                .into_iter()
                .nth(2)
                .is_some_and(|value| function.int_const_u128(value) == Some(u128::from(target)))
    })
}

/// Every `Call`'s constant target, for a failure message that names what was
/// seated instead.
fn call_targets(function: &strider_ir::Function) -> Vec<u128> {
    function
        .walk()
        .filter(|&nid| matches!(function.node_kind(nid), strider_ir::node::NodeKind::Call))
        .filter_map(|nid| {
            function
                .node_inputs(nid)
                .into_iter()
                .nth(2)
                .and_then(|value| function.int_const_u128(value))
        })
        .collect()
}

#[test]
fn orchestrator_owned_sleigh_succeeds_in_fast_path() {
    let bytes = vec![0xc3u8]; // ret
    let function = run_at(bytes, 0x1000)
        .expect("orchestrator must succeed in fast path")
        .function;
    let mut had_return = false;
    for nid in function.walk() {
        if matches!(function.node_kind(nid), strider_ir::node::NodeKind::Return) {
            had_return = true;
        }
    }
    assert!(
        had_return,
        "fast-path exit must produce a graph with at least one Return"
    );
}

#[test]
fn orchestrator_owned_sleigh_reports_unresolved_branch() {
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let result =
        run_at(bytes, 0x1000).expect("analyze returns Ok even with an unresolvable branch");
    assert!(
        !result.unresolved_indirect_branches.is_empty(),
        "the unresolvable `jmp rax` must be reported"
    );
}

#[test]
fn orchestrator_correctness_unchanged_after_sleigh_persistence() {
    // The graph for a function that needs no indirect resolution must be
    // identical regardless of how many times Sleigh is reused across runs.
    let make_run = || {
        let bytes = vec![0xc3u8]; // ret
        run_at(bytes, 0x1000).expect("orchestrator").function
    };
    let g1 = make_run();
    let g2 = make_run();

    let kinds_1: Vec<strider_ir::node::NodeKind> =
        g1.walk().map(|nid| *g1.node_kind(nid)).collect();
    let kinds_2: Vec<strider_ir::node::NodeKind> =
        g2.walk().map(|nid| *g2.node_kind(nid)).collect();
    assert_eq!(kinds_1, kinds_2);
}

#[test]
fn analyze_branch_behind_constant_false_guard_reports_clean() {
    // A `jmp rax` reachable only through a constant-false flag
    // (`xor eax,eax; test eax,eax; jne <indirect>`). The pipeline must
    // never leave a live, unclassified IndirectBranch for this shape, so
    // `analyze` reports an empty unresolved list (no spurious dead-branch
    // report). The underlying liveness/known-targets filter is exercised
    // directly by the `live_unresolved_*` unit tests in `lib.rs`; this is
    // the end-to-end check.
    //
    // 0x1000: 31 c0       xor eax,eax     (eax=0, ZF=1)
    // 0x1002: 85 c0       test eax,eax    (ZF stays 1)
    // 0x1004: 75 02       jne 0x1008      (taken iff ZF==0, never)
    // 0x1006: c3          ret             (live fallthrough)
    // 0x1008: ff e0       jmp rax         (guarded arm)
    let mut bytes: Vec<u8> = vec![0x31, 0xc0, 0x85, 0xc0, 0x75, 0x02, 0xc3, 0xff, 0xe0];
    bytes.extend(std::iter::repeat_n(0xc3u8, 16));
    let result = run_at(bytes, 0x1000).expect("analyze must succeed");
    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "no live, unclassified IndirectBranch -> empty unresolved list (got {:?})",
        result.unresolved_indirect_branches
    );
}

#[test]
fn analyze_resolution_loop_beats_single_pass_manual_lift() {
    // `push K; pop rax; jmp rax` resolves to a tail call only after the
    // classifier folds Single(K) into known_targets and the CFG is
    // rebuilt. A single manual `build_cfg + build_ir + pipeline.run` (what
    // `orchestrator_demo` does) leaves the IndirectBranch placeholder in
    // the graph; `Strider::analyze`'s resolve/re-lift loop removes it.
    use strider_orchestrator::Lifter;
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));

    // Single manual pass (the example's path).
    let arch = SleighArch::x86_64();
    let sleigh = make_sleigh_value(bytes.clone(), 0x1000);
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("cc");
    let mut lifter = Lifter::new(arch, sleigh).expect("lifter");
    let cfg = lifter
        .build_cfg(
            strider_cfg::MachineInsnAddr::from(0x1000u64),
            &Default::default(),
            &Default::default(),
        )
        .expect("cfg");
    let mut single = lifter
        .build_ir_with(&cfg, cc, &LiftOptions::default())
        .expect("ir")
        .function;
    let pipeline = strider_orchestrator::opt::default_pipeline();
    let mut ctx = strider_orchestrator::opt::OptCtx::new(None);
    pipeline.run(&mut single, &mut ctx).expect("pipeline");
    let single_pass_has_indirect = single.walk().any(|n| {
        matches!(
            single.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        single_pass_has_indirect,
        "a single manual lift+optimize pass should leave the IndirectBranch placeholder \
         (the example's path does not resolve it)"
    );

    // The orchestrator's resolution loop.
    let resolved = run_at(bytes, 0x1000)
        .expect("analyze must resolve the indirect tail call")
        .function;
    let loop_has_indirect = resolved.walk().any(|n| {
        matches!(
            resolved.node_kind(n),
            strider_ir::node::NodeKind::IndirectBranch
        )
    });
    assert!(
        !loop_has_indirect,
        "Strider::analyze's resolution loop must resolve the placeholder away"
    );
    let result_unresolved = {
        let arch = SleighArch::x86_64();
        let mut b2: Vec<u8> = vec![0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0];
        b2.extend(std::iter::repeat_n(0xccu8, 64));
        let sleigh = make_sleigh_value(b2, 0x1000);
        let regs = sleigh.regs().expect("regs");
        let cc = CallingConvention::x86_64_systemv()
            .build(&regs)
            .expect("cc");
        let mut strider = Strider::new(arch, sleigh, None).expect("strider");
        strider
            .analyze(
                0x1000,
                &cc,
                &LiftOptions::default(),
                &OptOptions::default(),
                None,
            )
            .expect("analyze")
            .unresolved_indirect_branches
    };
    assert!(
        result_unresolved.is_empty(),
        "fully-resolved tail call must report no unresolved branches"
    );
}

/// `links` x (`mov eax, next` ; `jmp rax`), then a `ret`, at `base`. Each link
/// is 7 bytes and its target is a literal, so the resolution loop discovers
/// EXACTLY one new link per re-lift: chain length is iteration count.
fn const_jump_chain(links: usize, base: u64) -> Vec<u8> {
    const LINK_LEN: u64 = 7;
    let mut bytes = Vec::with_capacity(links * 7 + 17);
    for i in 0..links {
        let next = base + (i as u64 + 1) * LINK_LEN;
        bytes.push(0xb8); // mov eax, imm32
        bytes.extend_from_slice(&u32::try_from(next).expect("32-bit address").to_le_bytes());
        bytes.extend_from_slice(&[0xff, 0xe0]); // jmp rax
    }
    bytes.push(0xc3); // ret
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    bytes
}

/// A chain shorter than the iteration cap resolves outright: the control for
/// the over-cap case below, so a failure there is the cap and not the shape.
#[test]
fn short_const_jump_chain_resolves_completely() {
    let result = run_at(const_jump_chain(32, 0x1000), 0x1000).expect("32-link chain must converge");
    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "a 32-link chain is inside the cap, so nothing may be left unresolved",
    );
}

/// The iteration cap doubles as the discovery-depth limit, so a chain longer
/// than it exhausts the budget while every site is still only GROWING. That is
/// a depth limit, not an oscillation: the analysis of the whole function must
/// not be lost.
#[test]
fn over_cap_const_jump_chain_degrades_instead_of_erroring() {
    let result = run_at(const_jump_chain(300, 0x1000), 0x1000)
        .expect("a chain deeper than the cap must degrade, not fail the whole analyze");
    assert!(
        !result.unresolved_indirect_branches.is_empty(),
        "the sites the budget ran out on must be REPORTED unresolved",
    );
    // The other half of the promise: the links the budget DID reach are still
    // analysed, so the reported sites are a suffix of the chain and not the
    // whole of it.
    let seated = result
        .cfg
        .regions()
        .filter(|region| {
            matches!(
                region.terminator,
                strider_cfg::RegionTerminator::Switch { .. }
            )
        })
        .count();
    assert!(
        seated >= MAX_RESOLUTION_ITERATIONS_FLOOR,
        "only {seated} links seated: the deep chain cost the whole function's \
         analysis instead of its tail",
    );
}

/// A floor on how much of an over-cap chain must survive: the loop seats one
/// link per round, so a 300-link chain resolves most of itself before the
/// budget runs out. Deliberately well under the real cap, which is private.
const MAX_RESOLUTION_ITERATIONS_FLOOR: usize = 200;
