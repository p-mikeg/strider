//! An address two edges reach in different ISA modes is decoded once, in
//! whichever mode won the work queue, so the losing edge's arm is not the
//! instruction stream it believes. The round that decoded it fed the
//! classifier, so a later round rebuilt without that edge must not launder the
//! report away.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, PcodeInsnAddr, ResolvedTarget, ResolvedTargets};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;
/// Reached as ARM by one seeded arm and as Thumb by the other.
const CLASH: u64 = 0x1020;

fn put(bytes: &mut [u8], at: u64, word: u32) {
    let off = (at - BASE) as usize;
    bytes[off..off + 4].copy_from_slice(&word.to_le_bytes());
}

/// ARM at 0x1000:
///
/// ```text
/// 1000  cmp r0, #0
/// 1004  beq 0x1014
/// 1008  bx r0             ; the seeded site
/// 1014  add pc, pc, #0    ; a constant dispatch to 0x101c
/// 101c  bx lr
/// 1020  the clash target
/// ```
///
/// Everything unwritten is `bx lr`, so no stray decode runs away.
fn bytes() -> Vec<u8> {
    let mut bytes = vec![0u8; 0x60];
    for i in 0..0x18 {
        put(&mut bytes, BASE + i * 4, 0xe12f_ff1e); // bx lr
    }
    put(&mut bytes, 0x1000, 0xe350_0000); // cmp r0, #0
    put(&mut bytes, 0x1004, 0x0a00_0002); // beq 0x1014
    put(&mut bytes, 0x1008, 0xe12f_ff10); // bx r0
    put(&mut bytes, 0x1014, 0xe28f_f000); // add pc, pc, #0
    // ARM `mov r4, r0, ror #14`; its low halfword is Thumb `bx lr`, so the
    // clash target terminates cleanly whichever mode decodes it.
    put(&mut bytes, CLASH, 0xe1a0_4770);
    bytes
}

/// The seed names one address twice, once per ISA mode, so seating it decodes
/// `CLASH` in one mode and reaches it in the other. `abandon_undecodable` then
/// drops the site, and the constant dispatch at 0x1014 forces a re-lift whose
/// cfg no longer carries the clashing edge at all.
#[test]
fn a_conflict_one_round_raised_survives_a_later_round_that_does_not() {
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
    known.insert(
        PcodeInsnAddr::at_machine_start(0x1008),
        ResolvedTargets::Multiple(vec![
            ResolvedTarget::new(CLASH, Some(false)),
            ResolvedTarget::new(CLASH, Some(true)),
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

    assert!(
        !result
            .cfg
            .regions()
            .any(|r| r.start_addr.machine_addr.addr == CLASH),
        "precondition: the final cfg must no longer reach {CLASH:#x} at all, \
         or nothing was laundered and the test proves nothing",
    );
    assert_eq!(
        result
            .isa_mode_conflicts
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![CLASH],
        "the round that decoded {CLASH:#x} twice fed the classifier; a later \
         round rebuilt without that edge cannot launder the report",
    );
}
