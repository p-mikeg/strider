//! Two edges reach 0x1010 in two ISA modes: a direct `b` from an ARM region,
//! and an interworking `bxeq` the classifier resolves with the Thumb bit set.
//!
//! The direct edge switches no mode, so the mode it carries is its parent
//! region's and certain; the arm's committed bit is a classifier claim. The
//! direct decode must own the bytes, the arm must go, and the published CFG
//! must agree with the report channels rather than still seating the `Switch`
//! the loop abandoned.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, PcodeInsnAddr, RegionTerminator, ResolvedTarget, ResolvedTargets};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;
const SITE: u64 = 0x1008;
const SHARED: u64 = 0x1010;

fn put(bytes: &mut [u8], at: u64, word: u32) {
    let off = (at - BASE) as usize;
    bytes[off..off + 4].copy_from_slice(&word.to_le_bytes());
}

/// ```text
/// 1000  add r3, pc, #9      ; r3 = 0x1011, the Thumb tag on 0x1010
/// 1004  cmp r0, #0
/// 1008  bxeq r3             ; interworking, resolves 0x1010 as Thumb
/// 100c  b 0x1010            ; direct ARM edge to the same address
/// 1010  mov r4, r0, ror #14 ; ARM; its low halfword 0x4770 is Thumb `bx lr`
/// ```
fn bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x40];
    for i in 0..0x10 {
        put(&mut bytes, BASE + i * 4, 0xe12f_ff1e); // bx lr
    }
    put(&mut bytes, 0x1000, 0xe28f_3009); // add r3, pc, #9
    put(&mut bytes, 0x1004, 0xe350_0000); // cmp r0, #0
    put(&mut bytes, SITE, 0x012f_ff13); // bxeq r3
    put(&mut bytes, 0x100c, 0xeaff_ffff); // b 0x1010
    put(&mut bytes, SHARED, 0xe1a0_4770); // mov r4, r0, ror #14
    bytes
}

#[test]
fn a_direct_edge_keeps_the_bytes_a_seeded_arm_claims_in_the_other_mode() {
    let arch = strider_target::SleighArch::arm();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes(), BASE),
    )
    .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("cc");

    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");

    // One build from the answer the classifier derives, so the arbitration is
    // pinned where it happens rather than through whatever the resolve loop
    // republishes.
    let mut known = rustc_hash::FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(SITE),
        ResolvedTargets::Multiple(vec![ResolvedTarget::new(SHARED, Some(true))]),
    );
    let seeded = strider
        .build_cfg(
            BASE,
            &CfgOptions {
                known_targets: known,
                ..CfgOptions::default()
            },
        )
        .expect("build_cfg");
    let claimed = seeded
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == SHARED)
        .expect("region at 0x1010");
    assert_eq!(
        claimed.insns.first().map(|i| i.len),
        Some(4),
        "the ARM decode is one 4-byte instruction and the Thumb one two bytes; \
         the direct edge's proved mode must beat the arm's claimed one",
    );
    assert!(
        !seeded
            .regions()
            .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. })),
        "the losing arm goes with the decode it lost, leaving the site deferred",
    );

    let result = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions {
                cfg: CfgOptions::default(),
                ..LiftOptions::default()
            },
            &OptOptions::default(),
            None,
        )
        .expect("a mode clash is a result, not an error");

    let shared = result
        .cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == SHARED)
        .expect("the direct branch must reach a region at 0x1010");
    assert_eq!(
        shared.insns.first().map(|i| i.len),
        Some(4),
        "the ARM decode is one 4-byte instruction and the Thumb one two bytes; \
         the direct edge's proved mode must own the bytes",
    );

    assert!(
        result
            .isa_mode_conflicts
            .iter()
            .any(|a| a.machine_addr.addr == SHARED),
        "the clash is still reported; got {:?}",
        result.isa_mode_conflicts,
    );
    assert!(
        !result
            .cfg
            .regions()
            .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. })),
        "the published CFG must not keep the seat the loop abandoned",
    );
    assert!(
        result
            .unresolved_indirect_branches
            .iter()
            .any(|a| a.machine_addr.addr == SITE),
        "the abandoned site is reported unresolved; got {:?}",
        result.unresolved_indirect_branches,
    );
    assert!(!result.is_complete());
}
