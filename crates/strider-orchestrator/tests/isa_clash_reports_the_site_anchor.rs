//! An ISA clash on a SEEDED arm reports the site by its `BRANCHIND` anchor.
//!
//! A caller can only spell the machine address, so its seat is keyed at p-code
//! index 0 while ARM `bx` puts the `BRANCHIND` later in the instruction. The
//! channels have to agree on one spelling of the site.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, PcodeInsnAddr, ResolvedTarget, ResolvedTargets};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;
/// The `bx r0`.
const DISPATCH: u64 = 0x1014;
/// Reached as ARM by the `beq` at 0x1004 and as Thumb by the `b` at 0x1030.
const CLASH: u64 = 0x1050;
const THUMB_ARM: u64 = 0x1030;
const KEPT_ARMS: [u64; 2] = [0x1060, 0x1070];

/// ARM at 0x1000, dispatching through a four-entry table the caller seeds.
///
/// ```text
/// 1000  cmp r2, #0
/// 1004  beq 0x1050               ; the ARM edge into the clash
/// 1008  and r0, r0, #3
/// 100c  ldr r1, [pc, #4]
/// 1010  ldr r0, [r1, r0, lsl #2]
/// 1014  bx r0                    ; the dispatch, seeded, no rom to derive from
/// 1018  .word 0x1020
/// 1020  .word 0x1031, 0x1050, 0x1060, 0x1070
/// 1030  (Thumb) b 0x1050         ; the Thumb edge into the clash
/// 1050  mov r4, r0, ror #14 ; bx lr
/// 1060  mov r0, #1 ; bx lr
/// 1070  mov r0, #3 ; bx lr
/// ```
fn bytes() -> Vec<u8> {
    let mut b = vec![0u8; 0x100];
    let mut put32 = |addr: u64, value: u32| {
        let at = (addr - BASE) as usize;
        b[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    put32(0x1000, 0xe352_0000); // cmp r2, #0
    put32(0x1004, 0x0a00_0011); // beq 0x1050
    put32(0x1008, 0xe200_0003); // and r0, r0, #3
    put32(0x100c, 0xe59f_1004); // ldr r1, [pc, #4]
    put32(0x1010, 0xe791_0100); // ldr r0, [r1, r0, lsl #2]
    put32(0x1014, 0xe12f_ff10); // bx r0
    put32(0x1018, 0x0000_1020); // table base
    put32(0x1020, 0x0000_1031); // arm 0, Thumb bit set
    put32(0x1024, 0x0000_1050); // arm 1, the clash
    put32(0x1028, 0x0000_1060); // arm 2
    put32(0x102c, 0x0000_1070); // arm 3
    put32(0x1030, 0x0000_e00e); // (Thumb) b 0x1050
    // ARM `mov r4, r0, ror #14`; its low halfword is Thumb `bx lr`, so 0x1050
    // terminates whichever mode decodes it.
    put32(0x1050, 0xe1a0_4770);
    put32(0x1054, 0xe12f_ff1e); // bx lr
    put32(0x1060, 0xe3a0_0001); // mov r0, #1
    put32(0x1064, 0xe12f_ff1e); // bx lr
    put32(0x1070, 0xe3a0_0003); // mov r0, #3
    put32(0x1074, 0xe12f_ff1e); // bx lr
    b
}

#[test]
fn a_clash_on_a_machine_start_seed_is_reported_at_the_branchind_anchor() {
    let arch = strider_target::SleighArch::arm();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes(), BASE),
    )
    .expect("sleigh");
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(&sleigh.regs().expect("regs"))
        .expect("cc");

    let mut known = rustc_hash::FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(DISPATCH),
        ResolvedTargets::Multiple(vec![
            ResolvedTarget::new(THUMB_ARM, Some(true)),
            ResolvedTarget::new(CLASH, None),
            ResolvedTarget::new(KEPT_ARMS[0], None),
            ResolvedTarget::new(KEPT_ARMS[1], None),
        ]),
    );
    let lift_opts = LiftOptions {
        cfg: CfgOptions {
            known_targets: known,
            ..CfgOptions::default()
        },
        ..LiftOptions::default()
    };

    // No rom: the table load does not fold, so the classifier stays silent and
    // the seed is the site's only answer.
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(BASE, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("a mode clash is a result, not an error");

    let anchor = result
        .cfg
        .regions()
        .find_map(|r| match &r.terminator {
            strider_cfg::RegionTerminator::Switch { addr, .. } => Some(*addr),
            _ => None,
        })
        .expect("the trimmed seat is still a Switch");
    assert_ne!(
        anchor.insn_index, 0,
        "precondition: ARM `bx` must put its BRANCHIND past p-code index 0, \
         or the seed key and the anchor cannot differ",
    );
    assert_eq!(
        result.isa_mode_conflicts,
        vec![PcodeInsnAddr::at_machine_start(CLASH)],
        "the clash is raised once, not once per round",
    );
    assert_eq!(
        result.unresolved_indirect_branches,
        vec![anchor],
        "the frozen site is named by its anchor, the address every other \
         channel uses",
    );
    assert_eq!(result.unverified_seeded_sites, vec![anchor]);
}
