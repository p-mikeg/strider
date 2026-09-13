//! An ISA clash raised by DIRECT flow costs only the arm naming the clashing
//! address, not the whole seated table.
//!
//! Two direct branches disagree about the mode of an address a derived jump
//! table also names. Nothing about the dispatch produced the clash, so it is no
//! verdict on the table's other arms.

mod common;

const BASE: u64 = 0x1000;
/// The `bx r0`, and the start of the region that seals it.
const DISPATCH: u64 = 0x1014;
const DISPATCH_REGION: u64 = 0x1008;
/// Reached as ARM by the `beq` at 0x1004 and as Thumb by the `b` at 0x1030.
const CLASH: u64 = 0x1050;
const THUMB_ARM: u64 = 0x1030;
const KEPT_ARMS: [u64; 2] = [0x1060, 0x1070];

/// ARM at 0x1000, dispatching through a four-entry interworking table.
///
/// ```text
/// 1000  cmp r2, #0
/// 1004  beq 0x1050               ; the ARM edge into the clash
/// 1008  and r0, r0, #3           ; KnownBits bounds the table at 4
/// 100c  ldr r1, [pc, #4]         ; r1 = 0x1020, the table base
/// 1010  ldr r0, [r1, r0, lsl #2]
/// 1014  bx r0                    ; the dispatch
/// 1018  .word 0x1020
/// 1020  .word 0x1031, 0x1050, 0x1060, 0x1070
/// 1030  (Thumb) b 0x1050         ; the Thumb edge into the clash
/// 1050  mov r4, r0, ror #14 ; bx lr    ; Thumb `bx lr` in its low halfword
/// 1060  ldr r0, [pc, #4] ; bx r0 ; .word 0x1080
/// 1070  mov r0, #3 ; bx lr
/// 1080  mov r0, #2 ; bx lr
/// ```
///
/// The nested dispatch at 0x1064 exists only once the table is seated, so it
/// resolves a round LATER than the clash: without a round after the one that
/// cut the arm, the loop converges on the cfg that still seats it.
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
    put32(0x1060, 0xe59f_0004); // ldr r0, [pc, #4]
    put32(0x1064, 0xe12f_ff10); // bx r0
    put32(0x106c, 0x0000_1080); // its one target
    put32(0x1070, 0xe3a0_0003); // mov r0, #3
    put32(0x1074, 0xe12f_ff1e); // bx lr
    put32(0x1080, 0xe3a0_0002); // mov r0, #2
    put32(0x1084, 0xe12f_ff1e); // bx lr
    b
}

#[test]
fn a_direct_flow_clash_costs_one_arm_and_leaves_the_table_seated() {
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes()));
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::Arm, bytes(), BASE, Some(rom));
    let result = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("a mode clash is a result, not an error");

    let dispatch = result
        .cfg
        .region_ids()
        .find(|&r| result.cfg.region_graph()[r].start_addr.machine_addr.addr == DISPATCH_REGION)
        .expect("the dispatch region");
    let strider_cfg::RegionTerminator::Switch { targets, .. } =
        &result.cfg.region_graph()[dispatch].terminator
    else {
        panic!(
            "the dispatch must still be seated, not demoted: {:?}",
            result.cfg.region_graph()[dispatch].terminator,
        );
    };
    assert_eq!(
        targets
            .iter()
            .map(|t| (t.addr, t.isa_bit))
            .collect::<Vec<_>>(),
        vec![
            (THUMB_ARM, Some(true)),
            (KEPT_ARMS[0], Some(false)),
            (KEPT_ARMS[1], Some(false)),
        ],
        "a clash raised by direct flow costs the arm naming {CLASH:#x} and \
         nothing else",
    );

    let successors: std::collections::BTreeSet<u64> = result
        .cfg
        .region_graph()
        .neighbors(dispatch)
        .map(|s| result.cfg.region_graph()[s].start_addr.machine_addr.addr)
        .collect();
    assert_eq!(
        successors,
        std::collections::BTreeSet::from([THUMB_ARM, KEPT_ARMS[0], KEPT_ARMS[1]]),
        "the surviving arms keep their edges",
    );
    assert_eq!(
        result
            .cfg
            .regions()
            .find(|r| r.start_addr.machine_addr.addr == THUMB_ARM)
            .and_then(|r| r.insns.first())
            .map(|i| i.len),
        Some(2),
        "the Thumb arm still decodes as Thumb",
    );
    assert!(
        result
            .cfg
            .regions()
            .any(|r| r.start_addr.machine_addr.addr == CLASH),
        "{CLASH:#x} is still reached by the `beq`; only the table arm went",
    );
    assert_eq!(
        result
            .isa_mode_conflicts
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![CLASH],
    );
    assert_eq!(
        result
            .unresolved_indirect_branches
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![DISPATCH],
        "a site cut down to the arms it could keep is not a complete answer",
    );
}
