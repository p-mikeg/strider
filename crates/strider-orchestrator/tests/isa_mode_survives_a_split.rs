//! A split must hand the first half the mode the region decoded in.
//!
//! `split_region` gives the second half a new start and builds a fresh region
//! for the first, which keeps the ORIGINAL start address. That is the address a
//! later edge arrives at, so if the first half carries no recorded mode the
//! clash check reads `None` and a wrong-mode arrival goes unreported, for no
//! reason other than that an unrelated target happened to split the region.
//!
//! Reaching it needs three seeded sites. The clash check runs during `explore`,
//! against whatever region currently owns the address, and one site's arms are
//! popped in ascending address order; since a split address always lies above
//! its region's start, a single site clashes before it ever splits. Only a
//! later site can arrive after the split has happened.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, PcodeInsnAddr, ResolvedTarget, ResolvedTargets};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;
/// Decoded as ARM, split by `INTERIOR`, then reached as Thumb from the third
/// site once it is only the FIRST HALF of the original region.
const SPLIT_REGION: u64 = 0x1020;
/// Interior to the region at `SPLIT_REGION`, so seating it splits that region.
const INTERIOR: u64 = 0x1024;

fn put(bytes: &mut [u8], at: u64, word: u32) {
    let off = (at - BASE) as usize;
    bytes[off..off + 4].copy_from_slice(&word.to_le_bytes());
}

/// Every filler is ARM `ror rN, r0, r7`, whose low halfword is Thumb `bx lr`,
/// so a Thumb arrival terminates instead of running off the buffer.
fn bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x60];
    for i in 0..0x18 {
        put(&mut bytes, BASE + i * 4, 0xe12f_ff1e); // bx lr
    }
    put(&mut bytes, 0x1000, 0xe350_0000); // cmp r0, #0
    put(&mut bytes, 0x1004, 0xe12f_ff10); // bx r0, first site
    put(&mut bytes, 0x1020, 0xe1a0_4770); // ror r4, r0, r7
    put(&mut bytes, 0x1024, 0xe1a0_5770); // ror r5, r0, r7
    put(&mut bytes, 0x1028, 0xe12f_ff1e); // bx lr
    put(&mut bytes, 0x1030, 0xe1a0_6770); // ror r6, r0, r7
    put(&mut bytes, 0x1034, 0xe12f_ff11); // bx r1, second site
    put(&mut bytes, 0x1038, 0xe1a0_7770); // ror r7, r0, r7
    put(&mut bytes, 0x103c, 0xe12f_ff12); // bx r2, third site
    bytes
}

fn arm(addr: u64) -> ResolvedTarget {
    ResolvedTarget::new(addr, Some(false))
}

#[test]
fn a_split_carries_the_isa_mode_to_the_first_half() {
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

    let mut known = rustc_hash::FxHashMap::default();
    // Decode SPLIT_REGION as ARM, and reach the site that splits it.
    known.insert(
        PcodeInsnAddr::at_machine_start(0x1004),
        ResolvedTargets::Multiple(vec![arm(SPLIT_REGION), arm(0x1030)]),
    );
    // Split SPLIT_REGION's region at INTERIOR, and reach the third site.
    known.insert(
        PcodeInsnAddr::at_machine_start(0x1034),
        ResolvedTargets::Multiple(vec![arm(INTERIOR), arm(0x1038)]),
    );
    // Only now, with the region already split, arrive at the first half in Thumb.
    known.insert(
        PcodeInsnAddr::at_machine_start(0x103c),
        ResolvedTargets::Multiple(vec![
            ResolvedTarget::new(SPLIT_REGION, Some(true)),
            arm(0x1048),
        ]),
    );
    let lift_opts = LiftOptions {
        cfg: CfgOptions {
            known_targets: known,
            ..CfgOptions::default()
        },
        ..LiftOptions::default()
    };

    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(BASE, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("a mode clash is a result, not an error");

    let starts: Vec<u64> = result
        .cfg
        .regions()
        .map(|r| r.start_addr.machine_addr.addr)
        .collect();
    assert!(
        starts.contains(&SPLIT_REGION) && starts.contains(&INTERIOR),
        "precondition: both halves must be present or no split happened; \
         got {starts:#x?}",
    );
    assert!(
        result
            .isa_mode_conflicts
            .iter()
            .any(|a| a.machine_addr.addr == SPLIT_REGION),
        "the first half kept {SPLIT_REGION:#x} as its start, so the Thumb \
         arrival there clashes with the ARM mode it decoded in; got {:?}",
        result
            .isa_mode_conflicts
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
    );
}
