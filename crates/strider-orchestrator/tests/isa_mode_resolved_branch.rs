#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::{Arch, driver_for_reader};
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, MachineInsnAddr};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

/// Lift `bytes` at `base` for `arch` (no resolution pass), returning the
/// function and its sole `IndirectBranch` placeholder.
fn lift_sole_indirect_branch(
    arch: Arch,
    bytes: Vec<u8>,
    base: u64,
) -> (strider_ir::Function, strider_ir::node::NodeId) {
    let reader = BufMemReader::new(bytes, base);
    let (mut driver, cc) = driver_for_reader(arch, reader);
    // The entry address carries the ISA-mode bit; the buffer stays at the even
    // `base`, which is where `build_cfg` masks the entry back to for decoding.
    let entry = match arch {
        Arch::ArmThumb => base | 1,
        _ => base,
    };
    let cfg = driver
        .build_cfg(
            MachineInsnAddr::from(entry),
            &CfgOptions::default(),
            &Default::default(),
        )
        .expect("build_cfg");
    let function = driver.build_ir(&cfg, cc).expect("build_ir").function;
    let branch = function
        .walk_kind(|k| matches!(k, NodeKind::IndirectBranch))
        .next()
        .expect("an unresolved indirect branch placeholder");
    (function, branch)
}

#[test]
fn thumb_bx_carries_its_setisamode_mode() {
    // Thumb `bx r0` (0x4700): BXWritePC(r0) inlines setISAMode(r0 & 1) and
    // branches to r0 & ~1.  r0 is unknown, so the branch defers and the
    // placeholder survives, carrying that mode as its input.
    let mut bytes = vec![0x00u8, 0x47]; // bx r0
    bytes.extend(std::iter::repeat_n(0u8, 64));
    let (function, branch) = lift_sole_indirect_branch(Arch::ArmThumb, bytes, 0x1000);

    let mode = function
        .indirect_branch_isa_mode(branch)
        .expect("interworking bx must carry its setISAMode mode");
    assert_ne!(
        mode,
        function.indirect_branch_target(branch),
        "the mode is the r0&1 interworking bit, not the r0&~1 target",
    );
}

#[test]
fn mips_jr_carries_its_isamodeswitch_mode() {
    // MIPS `jr $t9` (mips32be 0x03200008): JXWritePC writes ISAModeSwitch =
    // (t9 & 1) and branches to t9 & ~1. The write is captured through the
    // `ISAModeSwitch` varnode, not a setISAMode pcodeop.
    let mut bytes = vec![0x03u8, 0x20, 0x00, 0x08]; // jr $t9
    bytes.extend(std::iter::repeat_n(0u8, 64));
    let (function, branch) = lift_sole_indirect_branch(Arch::Mips32be, bytes, 0x1000);

    let mode = function
        .indirect_branch_isa_mode(branch)
        .expect("MIPS jr must carry its ISAModeSwitch mode");
    assert_ne!(
        mode,
        function.indirect_branch_target(branch),
        "the mode is the t9&1 interworking bit, not the t9&~1 target",
    );
}

#[test]
fn interworking_call_mode_does_not_leak_onto_a_later_non_switching_branch() {
    // Control: `bx r1` (0x4708) is interworking, so an r1-based `ISAModeSwitch`
    // write DOES produce a mode. This is the same write `blx r1` performs, so the
    // leak case below is not vacuous.
    let mut ctrl = vec![0x08u8, 0x47]; // bx r1
    ctrl.extend(std::iter::repeat_n(0u8, 64));
    let (ctrl_fn, ctrl_branch) = lift_sole_indirect_branch(Arch::ArmThumb, ctrl, 0x1000);
    assert!(
        ctrl_fn.indirect_branch_isa_mode(ctrl_branch).is_some(),
        "bx r1 must carry its r1&1 mode",
    );

    // The leak: `blx r1` (0x4788, a call) writes `ISAModeSwitch` at 0x1000,
    // committing the CALLEE's transient mode, and falls through; `mov pc, r0`
    // (0x4687, BranchWritePC) at 0x1002 is non-interworking and writes nothing.
    // The write and the branch are DIFFERENT instructions, so the branch must NOT
    // inherit the call's mode. The per-instruction address match is what stops
    // it; dropping the match makes this assertion fail.
    let mut bytes = vec![0x88u8, 0x47, 0x87, 0x46]; // blx r1 ; mov pc, r0
    bytes.extend(std::iter::repeat_n(0u8, 64));
    let (function, branch) = lift_sole_indirect_branch(Arch::ArmThumb, bytes, 0x1000);
    assert_eq!(
        function.indirect_branch_isa_mode(branch),
        None,
        "a call's committed mode must not leak onto a later non-switching branch",
    );
}

#[test]
fn plain_indirect_branch_carries_no_mode() {
    // aarch64 `br x0` (0xd61f0000): a plain register-indirect branch with no ISA
    // mode to commit, so it carries no mode input and a resolved target keeps
    // the flowing mode.
    let mut bytes = vec![0x00u8, 0x00, 0x1f, 0xd6]; // br x0
    bytes.extend(std::iter::repeat_n(0u8, 64));
    let (function, branch) = lift_sole_indirect_branch(Arch::Aarch64, bytes, 0x1000);

    assert_eq!(function.indirect_branch_isa_mode(branch), None);
}

// strider lifts each function in isolation on one reused Sleigh engine and
// pins the function's entry ISA mode out-of-band, from the address low bit. A
// prior function's mode forward-holds in the context database across a later
// function's cold entry, so the pin must also invalidate the disassembler's
// context cache or the later function decodes in the prior function's mode.

/// Writes a small two-instruction returning function of ISA mode `alt` (base
/// ISA when false, the address-low-bit alternate ISA when true) into
/// `bytes[at..]`.  Both modes decode cleanly to a `Return`; being different
/// instruction sets, they produce different pcode, so a mode leak changes the
/// decoded stream.
type FnWriter = fn(&mut [u8], usize, bool);

/// ARM (base) vs Thumb (alt), little-endian.
fn write_arm_thumb(bytes: &mut [u8], at: usize, thumb: bool) {
    if thumb {
        bytes[at..at + 2].copy_from_slice(&0x202au16.to_le_bytes()); // movs r0, #42
        bytes[at + 2..at + 4].copy_from_slice(&0x4770u16.to_le_bytes()); // bx lr
    } else {
        bytes[at..at + 4].copy_from_slice(&0xe3a0_0001u32.to_le_bytes()); // mov r0, #1
        bytes[at + 4..at + 8].copy_from_slice(&0xe12f_ff1eu32.to_le_bytes()); // bx lr
    }
}

/// MIPS32 (base) vs MIPS16e (alt, `RELP=1` in the pspec), big-endian.
fn write_mips_be(bytes: &mut [u8], at: usize, mips16: bool) {
    if mips16 {
        bytes[at..at + 2].copy_from_slice(&0x6a2au16.to_be_bytes()); // li $v0, 42
        bytes[at + 2..at + 4].copy_from_slice(&0xe820u16.to_be_bytes()); // jr $ra
    } else {
        bytes[at..at + 4].copy_from_slice(&0x03e0_0008u32.to_be_bytes()); // jr $ra
        bytes[at + 4..at + 8].copy_from_slice(&0u32.to_be_bytes()); // nop (delay slot)
    }
}

/// The flattened pcode of the function decoded at `addr`.  Mode-sensitive: the
/// same bytes decoded in the wrong ISA mode yield a different stream.
fn decode_stream<R: rsleigh::MemReader>(
    driver: &mut strider_orchestrator::Lifter<R>,
    addr: u64,
) -> Vec<rsleigh::Insn> {
    let cfg = driver
        .build_cfg(
            MachineInsnAddr::from(addr),
            &CfgOptions::default(),
            &Default::default(),
        )
        .expect("build_cfg");
    cfg.regions()
        .flat_map(|r| r.insns.iter().map(|ri| ri.insn.clone()))
        .collect()
}

/// Lifting `modes` (one function each, in address order) on a single reused
/// engine must decode each function exactly as lifting it alone on a pristine
/// engine does.  `write` lays down one function per mode; each is placed a page
/// apart so a prior function's mode forward-holds across the next entry.
fn reuse_is_leak_free(arch: Arch, write: FnWriter, modes: &[bool]) {
    let mut bytes = vec![0u8; 0x1000 * (modes.len() + 2)];
    let entries: Vec<u64> = modes
        .iter()
        .enumerate()
        .map(|(i, &alt)| {
            let at = 0x1000 * (i + 1);
            write(&mut bytes, at, alt);
            at as u64 | u64::from(alt)
        })
        .collect();

    // Reference: each function on its own pristine engine (no prior context).
    let alone: Vec<_> = entries
        .iter()
        .map(|&e| {
            let (mut d, _) = driver_for_reader(arch, BufMemReader::new(bytes.clone(), 0));
            decode_stream(&mut d, e)
        })
        .collect();

    // Reuse: all functions on one engine, in address order.
    let (mut driver, _) = driver_for_reader(arch, BufMemReader::new(bytes, 0));
    let reused: Vec<_> = entries
        .iter()
        .map(|&e| decode_stream(&mut driver, e))
        .collect();

    assert_eq!(
        reused, alone,
        "reused-engine decode diverged from the pristine decode for {arch:?} {modes:?}",
    );
}

/// Mode orderings that stress the cache in both leak directions: base-then-alt,
/// alt-then-base, runs of one mode before a flip, and irregular alternation.
const STRESS_SEQUENCES: &[&[bool]] = &[
    &[false, true],        // base forward-holds across an alt entry
    &[true, false],        // alt forward-holds across a base entry
    &[false, true, false], // flip back
    &[true, false, true],
    &[false, false, true],       // a run of base before the first alt
    &[true, true, false],        // a run of alt before the first base
    &[false, true, false, true], // steady alternation
    &[true, false, true, false, true, false],
    &[false, true, true, false, true, false, false, true], // irregular
];

#[test]
fn arm_thumb_switching_is_leak_free_under_engine_reuse() {
    for seq in STRESS_SEQUENCES {
        reuse_is_leak_free(Arch::Arm, write_arm_thumb, seq);
    }
}

#[test]
fn mips16_switching_is_leak_free_under_engine_reuse() {
    for seq in STRESS_SEQUENCES {
        reuse_is_leak_free(Arch::Mips32be, write_mips_be, seq);
    }
}

/// A function's internal branch target must decode in the function's own ISA
/// mode even when prior functions on the reused engine forward-painted (and
/// cached) the opposite mode over it.  Lifts `entry` alone on a pristine engine
/// and again after `priors`, requiring identical decodes.  Each `priors` layout
/// puts a same-mode function *between* the entry and the target so the entry's
/// forward paint stops short, leaving the target stale in the context store.
/// `get_context_at` (what `pin_at` reads) consults the context database while
/// the disassembler reads the cache, so the invariant holds only because each
/// mode pin invalidates the cache.
fn internal_branch_survives_reuse(arch: Arch, bytes: Vec<u8>, entry: u64, priors: &[u64]) {
    let (mut fresh, _) = driver_for_reader(arch, BufMemReader::new(bytes.clone(), 0));
    let alone = decode_stream(&mut fresh, entry);
    assert!(
        alone.len() > 1,
        "the function must span the branch and its target region",
    );

    let (mut driver, _) = driver_for_reader(arch, BufMemReader::new(bytes, 0));
    for &p in priors {
        let _ = decode_stream(&mut driver, p);
    }
    let reused = decode_stream(&mut driver, entry);

    assert_eq!(
        reused, alone,
        "{arch:?}: an internal branch target must decode in the function's own mode",
    );
}

#[test]
fn arm_internal_branch_into_thumb_painted_range_decodes_as_arm() {
    // An ARM function branches (plain `b`, no mode switch) to an ARM target
    // that two prior Thumb functions painted and cached as Thumb.
    let mut bytes = vec![0u8; 0x4000];
    let thumb_bx_lr = 0x4770u16.to_le_bytes();
    bytes[0x1000..0x1002].copy_from_slice(&thumb_bx_lr);
    bytes[0x2200..0x2202].copy_from_slice(&thumb_bx_lr);
    bytes[0x2000..0x2004].copy_from_slice(&0xea00_00feu32.to_le_bytes()); // b 0x2400
    bytes[0x2400..0x2404].copy_from_slice(&0xe3a0_0001u32.to_le_bytes()); // mov r0, #1
    bytes[0x2404..0x2408].copy_from_slice(&0xe12f_ff1eu32.to_le_bytes()); // bx lr
    internal_branch_survives_reuse(Arch::Arm, bytes, 0x2000, &[0x1001, 0x2201]);
}

#[test]
fn thumb_internal_branch_into_arm_painted_range_decodes_as_thumb() {
    // Mirror direction: a Thumb function branches to a Thumb target two prior
    // ARM functions painted/cached ARM.
    let mut bytes = vec![0u8; 0x4000];
    let arm_bx_lr = 0xe12f_ff1eu32.to_le_bytes();
    bytes[0x1000..0x1004].copy_from_slice(&arm_bx_lr);
    bytes[0x2200..0x2204].copy_from_slice(&arm_bx_lr);
    bytes[0x2000..0x2002].copy_from_slice(&0xe1feu16.to_le_bytes()); // b 0x2400 (Thumb)
    bytes[0x2400..0x2402].copy_from_slice(&0x202au16.to_le_bytes()); // movs r0, #42
    bytes[0x2402..0x2404].copy_from_slice(&0x4770u16.to_le_bytes()); // bx lr
    internal_branch_survives_reuse(Arch::Arm, bytes, 0x2001, &[0x1000, 0x2200]);
}

#[test]
fn arm_conditional_branch_target_in_thumb_painted_range_decodes_as_arm() {
    // A CondBranch enqueues BOTH successors; its taken target sits in a
    // prior-Thumb-painted range and must decode as ARM.
    let mut bytes = vec![0u8; 0x4000];
    let thumb_bx_lr = 0x4770u16.to_le_bytes();
    bytes[0x1000..0x1002].copy_from_slice(&thumb_bx_lr);
    bytes[0x2200..0x2202].copy_from_slice(&thumb_bx_lr);
    bytes[0x2000..0x2004].copy_from_slice(&0x1a00_00feu32.to_le_bytes()); // bne 0x2400
    bytes[0x2004..0x2008].copy_from_slice(&0xe3a0_0001u32.to_le_bytes()); // mov r0, #1
    bytes[0x2008..0x200c].copy_from_slice(&0xe12f_ff1eu32.to_le_bytes()); // bx lr
    bytes[0x2400..0x2404].copy_from_slice(&0xe3a0_0002u32.to_le_bytes()); // mov r0, #2
    bytes[0x2404..0x2408].copy_from_slice(&0xe12f_ff1eu32.to_le_bytes()); // bx lr
    internal_branch_survives_reuse(Arch::Arm, bytes, 0x2000, &[0x1001, 0x2201]);
}

/// A synthetic ARM function whose 4-entry jump table interworks: two arms carry
/// the Thumb bit, two do not.
///
/// ```text
/// 0x1000  and r0, r0, #3              ; KnownBits bounds the table at 4
/// 0x1004  ldr r1, [pc, #4]            ; r1 = 0x1020, the table base
/// 0x1008  ldr r0, [r1, r0, lsl #2]
/// 0x100c  bx r0                       ; interworking: mode = r0 & 1
/// 0x1010  .word 0x1020
/// 0x1020  .word 0x1031, 0x1041, 0x1050, 0x1060
/// 0x1030  movs r0, #42 ; bx lr        ; Thumb
/// 0x1040  movs r0, #43 ; bx lr        ; Thumb
/// 0x1050  mov r0, #1   ; bx lr        ; ARM
/// 0x1060  mov r0, #2   ; bx lr        ; ARM
/// ```
mod interworking_table {
    pub const BASE: u64 = 0x1000;
    pub const THUMB_ARMS: [u64; 2] = [0x1030, 0x1040];
    pub const ARM_ARMS: [u64; 2] = [0x1050, 0x1060];

    fn put32(bytes: &mut [u8], addr: u64, value: u32) {
        let at = (addr - BASE) as usize;
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put16(bytes: &mut [u8], addr: u64, value: u16) {
        let at = (addr - BASE) as usize;
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub fn bytes() -> Vec<u8> {
        let mut b = vec![0u8; 0x100];
        put32(&mut b, 0x1000, 0xe200_0003); // and r0, r0, #3
        put32(&mut b, 0x1004, 0xe59f_1004); // ldr r1, [pc, #4]
        put32(&mut b, 0x1008, 0xe791_0100); // ldr r0, [r1, r0, lsl #2]
        put32(&mut b, 0x100c, 0xe12f_ff10); // bx r0
        put32(&mut b, 0x1010, 0x0000_1020); // table base
        put32(&mut b, 0x1020, 0x0000_1031); // arm 0, Thumb bit set
        put32(&mut b, 0x1024, 0x0000_1041); // arm 1, Thumb bit set
        put32(&mut b, 0x1028, 0x0000_1050); // arm 2, ARM
        put32(&mut b, 0x102c, 0x0000_1060); // arm 3, ARM
        put16(&mut b, 0x1030, 0x202a); // movs r0, #42
        put16(&mut b, 0x1032, 0x4770); // bx lr
        put16(&mut b, 0x1040, 0x202b); // movs r0, #43
        put16(&mut b, 0x1042, 0x4770); // bx lr
        put32(&mut b, 0x1050, 0xe3a0_0001); // mov r0, #1
        put32(&mut b, 0x1054, 0xe12f_ff1e); // bx lr
        put32(&mut b, 0x1060, 0xe3a0_0002); // mov r0, #2
        put32(&mut b, 0x1064, 0xe12f_ff1e); // bx lr
        b
    }
}

/// The whole image, standing in for the ELF read-only view the classifier
/// folds table loads through.
struct BufRom {
    base: u64,
    bytes: Vec<u8>,
}

impl strider_orchestrator::opt::ReadOnlyMemory for BufRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let at = usize::try_from(
            addr.checked_sub(self.base)
                .ok_or_else(|| anyhow::anyhow!("BufRom: {addr:#x} below base"))?,
        )?;
        let src = self
            .bytes
            .get(at..at + buf.len())
            .ok_or_else(|| anyhow::anyhow!("BufRom: {addr:#x} unmapped"))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// The whole resolve/re-lift loop over the interworking table: the arms must
/// come back with the mode the `bx` proved for each, and the Thumb ones must
/// then DECODE as Thumb.
#[test]
fn analyze_resolves_an_interworking_table_in_each_arm_mode() {
    use interworking_table::{ARM_ARMS, BASE, THUMB_ARMS};

    let bytes = interworking_table::bytes();
    let sleigh_arch = Arch::Arm.sleigh();
    let sleigh = rsleigh::Sleigh::new(
        sleigh_arch.sla_spec(),
        sleigh_arch.pspec(),
        BufMemReader::new(bytes.clone(), BASE),
    )
    .expect("sleigh");
    let cc = Arch::Arm
        .cc()
        .build(&sleigh.regs().expect("regs"))
        .expect("cc");
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(BufRom {
        base: BASE,
        bytes: bytes.clone(),
    });
    let mut strider =
        strider_orchestrator::Strider::new(sleigh_arch, sleigh, Some(rom)).expect("Strider::new");
    let result = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze");

    assert!(
        result.unresolved_indirect_branches.is_empty(),
        "the masked interworking table must resolve: {:#x?}",
        result.unresolved_indirect_branches,
    );

    let mut arms: Vec<(u64, Option<bool>)> = result
        .cfg
        .regions()
        .filter_map(|r| match &r.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets),
            _ => None,
        })
        .flatten()
        .map(|t| (t.addr, t.isa_bit))
        .collect();
    arms.sort_unstable();
    arms.dedup();
    let expected: Vec<(u64, Option<bool>)> = THUMB_ARMS
        .iter()
        .map(|&a| (a, Some(true)))
        .chain(ARM_ARMS.iter().map(|&a| (a, Some(false))))
        .collect();
    let mut expected_sorted = expected.clone();
    expected_sorted.sort_unstable();
    assert_eq!(
        arms, expected_sorted,
        "each arm must carry the mode the `bx` proved for it",
    );

    // The mode is not decoration: a Thumb arm decodes 2-byte instructions and
    // an ARM arm 4-byte ones.
    let insn_len = |addr: u64| -> u32 {
        result
            .cfg
            .regions()
            .find(|r| r.start_addr.machine_addr.addr == addr)
            .unwrap_or_else(|| panic!("no region decoded at {addr:#x}"))
            .insns
            .first()
            .expect("a decoded instruction")
            .len
    };
    for arm in THUMB_ARMS {
        assert_eq!(insn_len(arm), 2, "{arm:#x} must decode as Thumb");
    }
    for arm in ARM_ARMS {
        assert_eq!(insn_len(arm), 4, "{arm:#x} must decode as ARM");
    }
}
