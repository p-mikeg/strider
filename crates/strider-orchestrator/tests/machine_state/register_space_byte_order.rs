//! The REGISTER space keeps the sla's byte order, which on ARM BE8 is not the
//! data order.
//!
//! `arm_be_kernel` pairs the little-endian sla with big-endian data, so its
//! register block is little-endian: `&d0 + 0` is the low half of `d0`. The
//! multi-structure `VLD2` / `VST2` bodies reach their D registers through a
//! loop-carried `*[register]` pointer, which the lifter models as loads and
//! stores in the REGISTER space. Those must be read in the register order, or
//! every element lands in the wrong half of its register.
//!
//! Each case runs the raw and the optimised IR through the reference
//! interpreter on `arm`, `arm_be_kernel` and `arm_be`. `arm_be` loads the
//! big-endian sla, whose register block is big-endian too, so `&d0 + 0` is the
//! HIGH half there: its answers follow that sla.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, MachineInsnAddr};
use strider_ir::node::NodeKind;
use strider_ir::{Function, ReadOnlyMemory, UBig};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::MockRom;
use strider_ir_test_utils::interp::{self, EntryVar, Env, Outcome, Slot};
use strider_orchestrator::Lifter;
use strider_target::{CallingConvention, Endianness, SleighArch};

const BASE: u64 = 0x1000;
const DATA: u64 = 0x2000;

const VLD2_32_D0_D1_R0: u32 = 0xF420_088F;
const VST2_32_D0_D1_R0: u32 = 0xF400_088F;
const VMOV_R2_R0_D0: u32 = 0xEC50_2B10;
const VMOV_D0_R1_R2: u32 = 0xEC42_1B10;
const VMOV_D1_R3_R4: u32 = 0xEC44_3B11;
const LDR_R1_R0_4: u32 = 0xE590_1004;
const LDR_R0_R0: u32 = 0xE590_0000;
const BX_LR: u32 = 0xE12F_FF1E;

/// `words` encoded in `order`.
fn bytes(words: &[u32], order: Endianness) -> Vec<u8> {
    words
        .iter()
        .flat_map(|w| match order {
            Endianness::Little => w.to_le_bytes(),
            Endianness::Big => w.to_be_bytes(),
        })
        .collect()
}

/// The raw and the optimised IR of `insns` at [`BASE`].
fn lift(
    arch: SleighArch,
    cc: &CallingConvention,
    insns: &[u32],
    rom: &dyn ReadOnlyMemory,
) -> [Function; 2] {
    // BE8 code is little-endian; everywhere else code follows the data.
    let code_order = if arch == SleighArch::arm_be_kernel() {
        Endianness::Little
    } else {
        arch.endianness()
    };
    let code = bytes(insns, code_order);
    let size = code.len() as u64;
    let mut lifter = Lifter::new(arch, BufMemReader::new(code, BASE)).expect("lifter");
    let cc = cc.build(lifter.sleigh_regs()).expect("cc");
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(size),
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(MachineInsnAddr::from(BASE), &opts)
        .expect("cfg");
    let raw = lifter.build_ir(&cfg, cc).expect("build_ir").function;
    let mut opt = raw.clone();
    let ctx = strider_orchestrator::opt::OptCtx::new(Some(rom));
    strider_orchestrator::opt::default_pipeline()
        .run(&mut opt, &ctx)
        .expect("pipeline");
    [raw, opt]
}

/// The first two returned registers and the RAM bytes written, of each of the
/// raw and the optimised run, with the named registers set at entry.
fn run_on(
    arch: SleighArch,
    cc: &CallingConvention,
    insns: &[u32],
    rom: &MockRom,
    regs: &[(&str, u128)],
) -> Vec<(u128, u128, std::collections::BTreeMap<u64, u8>)> {
    let names = arch.probe_regs();
    let at_entry: Vec<(rsleigh::Vn, u128)> = regs
        .iter()
        .map(|&(name, v)| (names.name_to_vn(name).expect("a register"), v))
        .collect();
    let entry = |var: EntryVar| {
        at_entry
            .iter()
            .find(|(vn, _)| *vn == var.vn)
            .map_or(UBig::ZERO, |&(_, v)| UBig::from(v))
    };
    let env = Env {
        entry: &entry,
        rom: Some(rom),
        seed: 1,
        step_budget: 1000,
        private: None,
        keeps: None,
    };
    lift(arch, cc, insns, rom)
        .iter()
        .map(|f| match interp::run(f, &env).expect("the function runs") {
            Outcome::Return { values, writes, .. } => match values.as_slice() {
                [Slot::Known(a), Slot::Known(b), ..] => (
                    u128::try_from(a).expect("a register value"),
                    u128::try_from(b).expect("a register value"),
                    writes,
                ),
                other => panic!("{arch:?}: the first two returns are not known: {other:?}"),
            },
            other => panic!("{arch:?}: ended {other:?}"),
        })
        .collect()
}

/// [`run_on`] under AAPCS, returning `r0` and `r1`.
fn run(
    arch: SleighArch,
    insns: &[u32],
    rom: &MockRom,
    regs: &[(&str, u32)],
) -> Vec<(u128, u128, std::collections::BTreeMap<u64, u8>)> {
    let regs: Vec<(&str, u128)> = regs.iter().map(|&(n, v)| (n, u128::from(v))).collect();
    run_on(arch, &CallingConvention::arm_aapcs(), insns, rom, &regs)
}

const W: [u32; 4] = [0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444];

/// `vld2.32 {d0,d1},[r0]; vmov r2,r0,d0` over memory words `W`: the returned
/// `r0` is lane 1 of `d0`.
fn vld2_lane1_of_d0(arch: SleighArch) -> Vec<u128> {
    let rom = MockRom::raw_bytes(DATA, bytes(&W, arch.endianness()));
    run(
        arch,
        &[VLD2_32_D0_D1_R0, VMOV_R2_R0_D0, BX_LR],
        &rom,
        &[("r0", DATA as u32)],
    )
    .into_iter()
    .map(|(r0, _, _)| r0)
    .collect()
}

/// VLD2 de-interleaves: lane 1 of `d0` is memory word 2.
#[test]
fn be8_vld2_puts_word_two_in_lane_one() {
    assert_eq!(
        vld2_lane1_of_d0(SleighArch::arm_be_kernel()),
        [u128::from(W[2]); 2],
        "the REGISTER space was read in the data order, taking the high half \
         of d0 where the little-endian register block means the low one"
    );
}

#[test]
fn little_endian_vld2_puts_word_two_in_lane_one() {
    assert_eq!(vld2_lane1_of_d0(SleighArch::arm()), [u128::from(W[2]); 2]);
}

/// The big-endian sla puts `&d0 + 0`, where its VLD2 body writes element 0, in
/// the high half.
#[test]
fn be32_vld2_follows_its_big_endian_register_block() {
    assert_eq!(
        vld2_lane1_of_d0(SleighArch::arm_be()),
        [u128::from(W[0]); 2]
    );
}

/// `vmov d0,r1,r2; vmov d1,r3,r4; vst2.32 {d0,d1},[r0]` then reads words 0
/// and 1 back into `r0` and `r1`. Returns `(r0, r1)` and the four written
/// words, per run.
fn vst2_round_trip(arch: SleighArch) -> Vec<(u128, u128, [u32; 4])> {
    const OUT: u64 = 0x5000;
    let rom = MockRom::raw_bytes(DATA, Vec::new());
    run(
        arch,
        &[
            VMOV_D0_R1_R2,
            VMOV_D1_R3_R4,
            VST2_32_D0_D1_R0,
            LDR_R1_R0_4,
            LDR_R0_R0,
            BX_LR,
        ],
        &rom,
        &[
            ("r0", OUT as u32),
            ("r1", W[0]),
            ("r2", W[1]),
            ("r3", W[2]),
            ("r4", W[3]),
        ],
    )
    .into_iter()
    .map(|(r0, r1, writes)| {
        let word = |i: u64| {
            let b: Vec<u8> = (0..4).map(|j| writes[&(OUT + 4 * i + j)]).collect();
            let b: [u8; 4] = b.try_into().expect("4 bytes");
            match arch.endianness() {
                Endianness::Little => u32::from_le_bytes(b),
                Endianness::Big => u32::from_be_bytes(b),
            }
        };
        (r0, r1, [word(0), word(1), word(2), word(3)])
    })
    .collect()
}

/// VST2 interleaves `d0 = r2:r1` and `d1 = r4:r3` as `r1, r3, r2, r4`.
#[test]
fn be8_vst2_round_trips_each_lane() {
    let want = (u128::from(W[0]), u128::from(W[2]), [W[0], W[2], W[1], W[3]]);
    assert_eq!(
        vst2_round_trip(SleighArch::arm_be_kernel()),
        [want; 2],
        "the REGISTER space was read in the data order, storing the high half \
         of each D register as its lane 0"
    );
}

#[test]
fn little_endian_vst2_round_trips_each_lane() {
    let want = (u128::from(W[0]), u128::from(W[2]), [W[0], W[2], W[1], W[3]]);
    assert_eq!(vst2_round_trip(SleighArch::arm()), [want; 2]);
}

/// Under the big-endian sla the VST2 body reads element 0 from the high half.
#[test]
fn be32_vst2_follows_its_big_endian_register_block() {
    let want = (u128::from(W[1]), u128::from(W[3]), [W[1], W[3], W[0], W[2]]);
    assert_eq!(vst2_round_trip(SleighArch::arm_be()), [want; 2]);
}

/// `lis r4,0xc000; b 1f; 1: mtsrin r3,r4; mfsr r3,3; blr`, with `r3` set.
///
/// The sla's `mtsrin` stores the whole `r3` at `&sr0 + (r4 >> 28)`, unscaled,
/// so a selector of 12 lands on `&sr3`, and on ppc64 the store is 8 bytes
/// wide over the 4-byte `sr3`. The branch puts `lis` in another region, so the
/// address is unknown at lift time and the store takes the opaque path; once
/// folded, the re-read of `sr3` is a narrower load at the store's address, and
/// `load_forward` narrows it in the REGISTER space's byte order: the high half
/// of `r3` on ppc64be, the low half on ppc64le.
fn mtsrin_then_mfsr(arch: SleighArch, cc: &CallingConvention, r3: u128) -> Vec<u128> {
    const PROGRAM: [u32; 5] = [
        0x3C80_C000,
        0x4800_0004,
        0x7C60_21E4,
        0x7C63_04A6,
        0x4E80_0020,
    ];
    let rom = MockRom::raw_bytes(DATA, Vec::new());
    let [raw, opt] = lift(arch, cc, &PROGRAM, &rom);
    let register_loads =
        |f: &Function| f.count_kind(|k| matches!(k, NodeKind::Load(rsleigh::VnSpace::REGISTER)));
    assert!(
        register_loads(&raw) > 0,
        "{arch:?}: the store must take the opaque path"
    );
    assert_eq!(
        register_loads(&opt),
        0,
        "{arch:?}: the re-read of sr3 was not forwarded from the opaque store"
    );
    run_on(arch, cc, &PROGRAM, &rom, &[("r3", r3)])
        .into_iter()
        .map(|(r3, _, _)| r3)
        .collect()
}

#[test]
fn ppc64be_narrows_a_register_space_load_to_the_high_half() {
    assert_eq!(
        mtsrin_then_mfsr(
            SleighArch::ppc64be(),
            &CallingConvention::powerpc64_elf_v1(),
            0x1122_3344_5566_7788
        ),
        [0x1122_3344; 2]
    );
}

#[test]
fn ppc64le_narrows_a_register_space_load_to_the_low_half() {
    assert_eq!(
        mtsrin_then_mfsr(
            SleighArch::ppc64le(),
            &CallingConvention::powerpc64_elf_v2(),
            0x1122_3344_5566_7788
        ),
        [0x5566_7788; 2]
    );
}

/// On ppc32 the store is as wide as `sr3`, so it forwards whole.
#[test]
fn ppc32be_forwards_a_register_space_store_of_the_same_width() {
    assert_eq!(
        mtsrin_then_mfsr(
            SleighArch::ppc32be(),
            &CallingConvention::powerpc_sysv32(),
            0x1122_3344
        ),
        [0x1122_3344; 2]
    );
}
