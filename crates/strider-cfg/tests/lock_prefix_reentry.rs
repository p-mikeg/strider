//! An x86 branch to an address one byte into a `lock`-prefixed instruction
//! (glibc's `je 0f; lock; 0:`) re-enters the same instruction's decode. The
//! edge enters at the `lock` instruction, split out of its region, so it is
//! exact and not reported as interior.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Cfg, CfgOptions, DecodeEngine};
use strider_target::SleighArch;

fn build_x86(bytes: Vec<u8>) -> Cfg {
    let arch = SleighArch::x86();
    DecodeEngine::new(arch, BufMemReader::new(bytes, 0x1000))
        .expect("decode engine")
        .build_cfg(0x1000u64.into(), &CfgOptions::default())
        .expect("build")
}

fn interior(cfg: &Cfg) -> Vec<u64> {
    cfg.incomplete()
        .of(strider_cfg::Reason::InteriorTarget)
        .map(|a| a.machine())
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
        cfg.region_graph()[taken].start_addr.machine(),
        0x1002,
        "the taken edge lands on the region the `lock` instruction starts",
    );
}

#[test]
fn a_branch_over_a_lock_prefix_mid_region_enters_at_the_lock_instruction() {
    // An instruction between the branch's fall-through and the `lock`: the
    // taken edge splits it off and enters at the `lock` instruction.
    //
    // 0x1000 je 0x1005              (74 03)
    // 0x1002 xor %eax,%eax          (31 c0)
    // 0x1004 lock xadd %ecx,(%eax)  (f0 0f c1 08) -- target 0x1005
    // 0x1008 ret                    (c3)
    let cfg = build_x86(vec![0x74, 0x03, 0x31, 0xc0, 0xf0, 0x0f, 0xc1, 0x08, 0xc3]);
    assert!(interior(&cfg).is_empty(), "{:?}", interior(&cfg));
    assert_eq!(
        taken_start(&cfg),
        strider_cfg::PcodeInsnAddr::at_machine_start(0x1004)
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

/// The region holding the start of the instruction the taken edge lands in.
fn taken_start(cfg: &Cfg) -> strider_cfg::PcodeInsnAddr {
    let taken = cfg
        .region_if(cfg.entry())
        .expect("entry is a CondBranch")
        .if_true_region
        .expect("taken arm");
    cfg.region_graph()[taken].start_addr
}

#[test]
fn a_branch_over_the_lock_of_a_multi_region_cmpxchg_is_not_interior() {
    // `cmpxchg` lifts with internal p-code branches, so it spans several
    // regions; the taken edge must land on its first op.
    //
    // 0x1000 je 0x1003                  (74 01)
    // 0x1002 lock cmpxchg %ecx,(%edi)   (f0 0f b1 0f)
    // 0x1006 ret                        (c3)
    let cfg = build_x86(vec![0x74, 0x01, 0xf0, 0x0f, 0xb1, 0x0f, 0xc3]);
    assert!(interior(&cfg).is_empty(), "{:?}", interior(&cfg));
    assert_eq!(
        taken_start(&cfg),
        strider_cfg::PcodeInsnAddr::at_machine_start(0x1002)
    );
}

#[test]
fn a_branch_into_a_rep_string_op_lands_on_its_first_op() {
    // `rep movsb` loops in p-code; a target past the `rep` is not the same
    // instruction, so it is reported, but the inexact edge still runs the
    // whole instruction rather than its last p-code slice.
    //
    // 0x1000 je 0x1003    (74 01)
    // 0x1002 rep movsb    (f3 a4)
    // 0x1004 ret          (c3)
    let cfg = build_x86(vec![0x74, 0x01, 0xf3, 0xa4, 0xc3]);
    assert_eq!(interior(&cfg), vec![0x1003]);
    assert_eq!(
        taken_start(&cfg),
        strider_cfg::PcodeInsnAddr::at_machine_start(0x1002)
    );
}

#[test]
fn a_lock_reentry_report_is_independent_of_visit_order() {
    // Both reach `lock cmpxchg` at 0x1004 and the bare `cmpxchg` at 0x1005;
    // only which edge the work queue takes first differs.
    //
    // 0x1000 jne 0x1009; 0x1002 jmp 0x1005; 0x1004 lock cmpxchg; 0x1008 ret;
    // 0x1009 jmp 0x1004
    let unlocked_first = vec![
        0x75, 0x07, 0xeb, 0x01, 0xf0, 0x0f, 0xb1, 0x0f, 0xc3, 0xeb, 0xf9,
    ];
    // 0x1000 jne 0x1009; 0x1002 jmp 0x1004; 0x1004 lock cmpxchg; 0x1008 ret;
    // 0x1009 jmp 0x1005
    let locked_first = vec![
        0x75, 0x07, 0xeb, 0x00, 0xf0, 0x0f, 0xb1, 0x0f, 0xc3, 0xeb, 0xfa,
    ];
    let a = build_x86(unlocked_first).incomplete().to_string();
    let b = build_x86(locked_first).incomplete().to_string();
    assert_eq!(a, b);
    assert_eq!(a, "");
}

#[test]
fn a_lock_reentry_behind_an_earlier_instruction_is_benign_in_either_order() {
    // As above, with `xor %eax,%eax` at 0x1004 heading the `lock` region:
    // either order splits at the `lock`, so the 0x1007 re-entry is benign.
    //
    // 0x1000 jne 0x100b; 0x1002 jmp 0x1007; 0x1004 xor; 0x1006 lock cmpxchg;
    // 0x100a ret; 0x100b jmp 0x1004
    let unlocked_first = vec![
        0x75, 0x09, 0xeb, 0x03, 0x31, 0xc0, 0xf0, 0x0f, 0xb1, 0x0f, 0xc3, 0xeb, 0xf7,
    ];
    // 0x1002 jmp 0x1004; 0x100b jmp 0x1007
    let locked_first = vec![
        0x75, 0x09, 0xeb, 0x00, 0x31, 0xc0, 0xf0, 0x0f, 0xb1, 0x0f, 0xc3, 0xeb, 0xfa,
    ];
    for bytes in [unlocked_first, locked_first] {
        assert_eq!(interior(&build_x86(bytes)), Vec::<u64>::new());
    }
}
