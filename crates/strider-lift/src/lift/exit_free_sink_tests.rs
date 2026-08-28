use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use super::handler_tests::lift_bytes;

fn x86_64_fn(bytes: Vec<u8>) -> strider_ir::Function {
    lift_bytes(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        bytes,
    )
    .expect("fixture must lift")
}

fn count_kind(f: &strider_ir::Function, want: fn(&NodeKind) -> bool) -> usize {
    f.graph()
        .all_node_ids()
        .filter(|n| want(f.node_kind(*n)))
        .count()
}

fn stores(f: &strider_ir::Function) -> usize {
    count_kind(f, |k| matches!(k, NodeKind::Store(_)))
}

fn sinks(f: &strider_ir::Function) -> usize {
    count_kind(f, |k| matches!(k, NodeKind::Unreachable))
}

/// `add eax,1 ; mov [0x2000],eax ; jmp f`: the cycle has no exit, so nothing
/// anchors its memory chain and compaction takes the whole body with it.
#[test]
fn exit_free_loop_keeps_its_store_through_compaction() {
    let mut f = x86_64_fn(vec![
        0x83, 0xc0, 0x01, // add $1,%eax
        0x89, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00, // mov %eax,0x2000
        0xeb, 0xf4, // jmp f
    ]);
    assert_eq!(sinks(&f), 1, "the exit-free cycle gets exactly one sink");
    assert_eq!(stores(&f), 1);
    f.compact().expect("compaction");
    assert_eq!(stores(&f), 1, "the store survives compaction");
    assert_eq!(sinks(&f), 1, "the sink survives compaction");
}

/// A three-region exit-free cycle whose join carries a loop-carried `MemPhi`:
/// `add eax,1 ; cmp eax,5 ; jne L2 ; mov [0x2000],eax ; L2: jmp f`.
#[test]
fn multi_region_exit_free_cycle_keeps_its_store() {
    let mut f = x86_64_fn(vec![
        0x83, 0xc0, 0x01, // add $1,%eax
        0x83, 0xf8, 0x05, // cmp $5,%eax
        0x75, 0x07, // jne L2
        0x89, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00, // mov %eax,0x2000
        0xeb, 0xef, // L2: jmp f
    ]);
    assert!(
        count_kind(&f, |k| matches!(k, NodeKind::MemPhi)) >= 1,
        "the join carries a loop-carried MemPhi"
    );
    assert_eq!(sinks(&f), 1, "one sink for the one exit-free cycle");
    f.compact().expect("compaction");
    assert_eq!(stores(&f), 1, "the store survives compaction");
}

/// A function that returns is left alone.
#[test]
fn returning_function_gets_no_sink() {
    let f = x86_64_fn(vec![
        0x83, 0xc0, 0x01, // add $1,%eax
        0xc3, // ret
    ]);
    assert_eq!(sinks(&f), 0, "a returning function needs no sink");
    assert_eq!(
        count_kind(&f, |k| matches!(k, NodeKind::If)),
        0,
        "no branch is seated on a returning function"
    );
}

/// A loop that DOES exit is left alone: the exit reaches the `Return`.
#[test]
fn exiting_loop_gets_no_sink() {
    // 0x1001 is interior to the `add`, so the back edge lands on the region
    // that owns those bytes rather than on an instruction boundary.
    let f = x86_64_fn(vec![
        0x83, 0xc0, 0x01, // add $1,%eax
        0x83, 0xf8, 0x05, // cmp $5,%eax
        0x75, 0xf9, // jne 0x1001
        0xc3, // ret
    ]);
    assert_eq!(sinks(&f), 0, "a loop with an exit needs no sink");
}

/// `test eax,eax ; jne out ; L: jmp L ; out: ret`: the sink belongs to the
/// self-jump at 0x1004, not to the function's first instruction.
#[test]
fn seated_sink_is_fingerprinted_to_the_back_branch() {
    let f = x86_64_fn(vec![
        0x85, 0xc0, // test %eax,%eax
        0x75, 0x02, // jne out
        0xeb, 0xfe, // L: jmp L
        0xc3, // out: ret
    ]);
    let sink = f
        .graph()
        .all_node_ids()
        .find(|n| matches!(f.node_kind(*n), NodeKind::Unreachable))
        .expect("the exit-free cycle gets a sink");
    let fingerprint = f.side_tables().asm_fingerprint(sink);
    assert!(
        fingerprint.contains(&0x1004),
        "the sink must name the back branch at 0x1004; got {fingerprint:?}"
    );
    assert!(
        !fingerprint.contains(&0x1000),
        "the sink must not name the function entry at 0x1000; got {fingerprint:?}"
    );
}

/// One `test / jne / jmp $` block per exit-free cycle, then a `ret`.
fn spin_loops(count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..count {
        bytes.extend_from_slice(&[
            0x85, 0xc0, // test %eax,%eax
            0x75, 0x02, // jne next block
            0xeb, 0xfe, // jmp $
        ]);
    }
    bytes.push(0xc3);
    bytes
}

/// The node visits sink seating performs, and the sinks it seated.
fn seating_cost(count: usize) -> (usize, usize) {
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(spin_loops(count), 0x1000),
    )
    .expect("create Sleigh");
    let mut lifter = crate::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cfg = lifter
        .build_cfg(
            0x1000u64.into(),
            &strider_cfg::CfgOptions::default(),
            &rustc_hash::FxHashMap::default(),
        )
        .expect("cfg");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let (outcome, visits) = lifter
        .build_ir_counting_sink_visits(&cfg, cc, &crate::lift::LiftOptions::default())
        .expect("lift");
    (visits, sinks(&outcome.function))
}

/// Seating scans the whole function once and drops the escaped nodes
/// incrementally, so doubling the cycles at most doubles the work.
#[test]
fn sink_seating_cost_is_not_quadratic_in_the_cycle_count() {
    let (small, small_sinks) = seating_cost(4);
    let (big, big_sinks) = seating_cost(8);
    assert_eq!(small_sinks, 4, "one sink per spin loop");
    assert_eq!(big_sinks, 8, "one sink per spin loop");
    assert!(
        big <= 2 * small,
        "doubling the cycles must not more than double the work; \
         4 cycles cost {small} visits, 8 cost {big}"
    );
}
