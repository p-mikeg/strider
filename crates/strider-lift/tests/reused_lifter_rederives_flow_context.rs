//! A reused `Lifter` decodes a function exactly as a fresh one does.
//!
//! Pins the production wiring: `Lifter::build_cfg` lends
//! `SleighArch::flow_commit_vars` to the builder, so a value a previous
//! `build_cfg` committed on this engine is re-derived rather than believed.
//! Dropping that lend leaves the whole flow-commit mechanism inert, which no
//! test over a hand-built `Builder` can see.
//!
//! One case per flow class: `condit` is `noflow`, `TEEMode` flows.

const BASE: u64 = 0x1000;
/// `itt eq` at 0x1000 commits `condit` at 0x1002 and predicates two
/// instructions. Entering at 0x1002 must decode `movs` unpredicated.
const F1: u64 = 0x1001;
const F2: u64 = 0x1003;

fn bytes() -> Vec<u8> {
    // 1000 itt eq | 1002 movs r0,#0 | 1004 movs r0,#0 | 1006 bx lr
    let mut v = vec![0x04, 0xbf, 0x00, 0x20, 0x00, 0x20, 0x70, 0x47];
    v.resize(0x40, 0);
    v
}

fn lifter() -> strider_lift::lift::Lifter<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    let arch = strider_target::SleighArch::arm_thumb();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes(), BASE),
    )
    .expect("sleigh");
    strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter")
}

/// Every machine instruction a build decoded, as opcode names per address.
fn build(
    l: &mut strider_lift::lift::Lifter<rsleigh::mem_readers::BufMemReader<Vec<u8>>>,
    at: u64,
) -> strider_cfg::Cfg {
    l.build_cfg(
        strider_cfg::MachineInsnAddr { addr: at },
        &Default::default(),
        &rustc_hash::FxHashMap::default(),
    )
    .expect("build_cfg")
}

fn decode(cfg: &strider_cfg::Cfg) -> Vec<(u64, Vec<String>)> {
    let mut out: Vec<(u64, Vec<String>)> = cfg
        .regions()
        .flat_map(|r| r.insns.iter())
        .map(|i| {
            (
                i.addr.machine_addr.addr,
                vec![format!("{:?}", i.insn.opcode)],
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_reused_lifter_decodes_the_second_function_as_a_fresh_one_does() {
    let fresh = decode(&build(&mut lifter(), F2));

    let mut reused = lifter();
    build(&mut reused, F1);
    let after = decode(&build(&mut reused, F2));

    // Non-empty, so a regression that emptied every CFG cannot pass this.
    assert!(!fresh.is_empty(), "the fresh build decoded nothing");
    assert_eq!(
        after, fresh,
        "0x1002's decode changed because an earlier build_cfg on this engine \
         committed `condit` there"
    );
}

/// Bytes for the ThumbEE tests: `enterx` commits `TEEMode=1` at 0x1004, under
/// which `0xc201` / `0xc202` are the zero-p-code-op `hb #1` / `hb #2` rather
/// than `stmia`s with a STORE.
fn thumbee_bytes() -> Vec<u8> {
    // 1000 enterx | 1004 hb #1 | 1006 hb #2 | 1008 bx lr
    let mut v = vec![0xbf, 0xf3, 0x1f, 0x8f, 0x01, 0xc2, 0x02, 0xc2, 0x70, 0x47];
    v.resize(0x40, 0);
    v
}

fn thumbee_lifter() -> strider_lift::lift::Lifter<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    let arch = strider_target::SleighArch::arm_thumb();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(thumbee_bytes(), BASE),
    )
    .expect("sleigh");
    strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter")
}

/// A region owning `at` that records no p-code op and spans `span` bytes,
/// which is what a zero-op ThumbEE `hb` looks like. Asserted positively
/// because "no STORE anywhere" also holds when the bytes stop being decoded
/// at all.
fn assert_zero_op_region(cfg: &strider_cfg::Cfg, at: u64, span: u32) {
    let r = cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == at)
        .unwrap_or_else(|| panic!("no region starts at {at:#x}"));
    assert_eq!(
        (r.insns.len(), r.empty_span_len),
        (0, span),
        "{at:#x} is not a zero-op ThumbEE instruction"
    );
}

/// `TEEMode` is FLOWING (`ARM.sinc:105`), so it holds over the whole ThumbEE
/// region. Erasing it to the pspec DEFAULT at each `inst_next` instead of to
/// the predecessor's value re-establishes it only where `enterx` is the
/// immediate predecessor, decoding the SECOND ThumbEE instruction on as plain
/// Thumb: here a `stmia`, inventing a memory write.
#[test]
fn thumbee_mode_flows_past_the_instruction_after_enterx() {
    let cfg = build(&mut thumbee_lifter(), 0x1001);
    assert_zero_op_region(&cfg, 0x1004, 2);
    assert_zero_op_region(&cfg, 0x1006, 2);
}

/// The same var in the other direction. `TEEMode` is flowing, so an earlier
/// build's commit sets a CHANGE POINT at that address, which a region-start
/// `FlowVars::pin_at` paints up to and not over. Only re-deriving it clears
/// the stale mode, and without that a later build reads the first build's
/// ThumbEE state and drops the `stmia`'s STORE entirely.
#[test]
fn an_earlier_builds_thumbee_mode_does_not_reach_this_build() {
    let fresh = decode(&build(&mut thumbee_lifter(), 0x1003));

    let mut reused = thumbee_lifter();
    build(&mut reused, 0x1001); // commits TEEMode=1 at 0x1004
    let after = decode(&build(&mut reused, 0x1003));

    assert!(
        fresh
            .iter()
            .any(|(_, ops)| ops.iter().any(|o| o == "Store")),
        "the fresh build should decode 0x1004 as a plain-Thumb store: {fresh:?}"
    );
    assert_eq!(
        after, fresh,
        "0x1004 decoded as ThumbEE because an earlier build_cfg committed \
         `TEEMode` there"
    );
}
