//! An x86 branch to an address one byte into a `lock`-prefixed instruction
//! re-enters the same instruction's decode. The edge is exact, and not
//! reported as interior, only while the region owning those bytes STARTS at
//! the `lock` instruction, which is glibc's `je 0f; lock; 0:` shape. An owner
//! starting earlier runs instructions the taken path never executes.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions};
use strider_target::SleighArch;

fn build_x86(bytes: Vec<u8>) -> Cfg {
    build_x86_with(bytes, &[])
}

/// `flow_commit_defaults` lent as `Lifter::build_cfg` lends it. Non-empty
/// means this build re-derives committed context and `Builder::enqueue`
/// therefore believes a snapshot of the engine, which is what bars the
/// reentry probe's throwaway decode.
fn build_x86_with(
    bytes: Vec<u8>,
    flow_commit_defaults: &[(&'static str, strider_target::FlowCommit, u32)],
) -> Cfg {
    let arch = SleighArch::x86();
    let mut sleigh = Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    Builder::for_arch(&arch, &mut sleigh, 0x1000, &CfgOptions::default())
        .with_flow_commit_defaults(flow_commit_defaults)
        .build()
        .expect("build")
}

/// The `lock`-prefix shape above, which is the only one that reaches the
/// probe's decode and accepts it.
const LOCK_REENTRY: [u8; 9] = [0x74, 0x01, 0xf0, 0x0f, 0xc1, 0x08, 0x89, 0xc8, 0xc3];

fn interior(cfg: &Cfg) -> Vec<u64> {
    cfg.interior_branch_targets()
        .iter()
        .map(|a| a.machine_addr.addr)
        .collect()
}

#[test]
fn a_branch_over_a_lock_prefix_its_own_region_starts_at_is_not_interior() {
    // glibc's `LOCK` macro: the `lock` byte is the branch's own fall-through,
    // so it starts the region the taken edge lands one byte into.
    //
    // 0x1000 je 0x1003              (74 01)
    // 0x1002 lock xadd %ecx,(%eax)  (f0 0f c1 08) -- target 0x1003 is the bare xadd
    // 0x1006 mov %ecx,%eax          (89 c8)
    // 0x1008 ret                    (c3)
    let cfg = build_x86(vec![0x74, 0x01, 0xf0, 0x0f, 0xc1, 0x08, 0x89, 0xc8, 0xc3]);
    assert!(
        interior(&cfg).is_empty(),
        "a lock-prefix re-entry into the owner's first instruction is exact: {:?}",
        interior(&cfg),
    );
    let taken = cfg
        .region_if(cfg.entry())
        .expect("entry is a CondBranch")
        .if_true_region
        .expect("taken arm");
    assert_eq!(
        cfg.region_graph()[taken].start_addr.machine_addr.addr,
        0x1002,
        "the taken edge lands on the region the `lock` instruction starts",
    );
}

#[test]
fn a_branch_over_a_lock_prefix_an_earlier_region_owns_is_reported() {
    // An instruction between the branch's fall-through and the `lock` puts the
    // owner's start before the containing instruction, so the taken path would
    // run that instruction too.
    //
    // 0x1000 je 0x1005              (74 03)
    // 0x1002 xor %eax,%eax          (31 c0)  fall-through, and the owner's start
    // 0x1004 lock xadd %ecx,(%eax)  (f0 0f c1 08) -- target 0x1005
    // 0x1008 ret                    (c3)
    let cfg = build_x86(vec![0x74, 0x03, 0x31, 0xc0, 0xf0, 0x0f, 0xc1, 0x08, 0xc3]);
    assert_eq!(
        interior(&cfg),
        vec![0x1005],
        "an owner starting before the `lock` instruction is not an exact edge",
    );
}

#[test]
fn a_branch_into_a_real_instruction_interior_is_still_reported() {
    // 0x1000 je 0x1005 (74 03) into the middle of the 5-byte `mov eax, imm32`
    // at 0x1004, which is not a prefix re-entry.
    let bytes = vec![
        0x74, 0x03, // 0x1000 je 0x1005
        0x31, 0xc9, // 0x1002 xor ecx,ecx
        0xb8, 0x11, 0x22, 0x33, 0x44, // 0x1004 mov eax, 0x44332211 (target 0x1005 interior)
        0xc3, // 0x1009 ret
    ];
    let cfg = build_x86(bytes);
    assert_eq!(
        interior(&cfg),
        vec![0x1005],
        "a branch into a non-prefix instruction interior is a real overlap"
    );
}

/// The probe lifts a MID-INSTRUCTION address out of band, and `oneInstruction`
/// applies that constructor's `globalset`s with no `RegionBuilder` to
/// re-derive them. So it must not run where `Builder::enqueue` believes a
/// snapshot of the engine, which it does exactly where this build re-derives
/// committed context.
///
/// Asserted on the difference the guard makes, not on a `false` return: the
/// probe answers `false` for almost any address, so a test that only checks
/// `false` passes whether the guard is there or not.
#[test]
fn lending_committed_context_bars_the_reentry_probe() {
    assert!(
        interior(&build_x86_with(LOCK_REENTRY.to_vec(), &[])).is_empty(),
        "with nothing to re-derive the probe runs and the reentry is exact",
    );
    // `segover` is a real x86 flowing var (`ia.sinc:411`) that no constructor
    // commits, standing in for an arch that does commit one: what is under
    // test is the guard, not this var. It has to exist in the sla, because
    // `rederive_flow` reads every name it is lent.
    let lent = [("segover", strider_target::FlowCommit::NoFlow, 0u32)];
    assert_eq!(
        interior(&build_x86_with(LOCK_REENTRY.to_vec(), &lent)),
        vec![0x1003],
        "with committed context to re-derive the probe must be barred, so the \
         reentry falls back to being reported as interior",
    );
}
