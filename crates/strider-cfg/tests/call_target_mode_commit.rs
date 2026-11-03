//! An interworking call (`blx #imm`, MIPS `jalx`) commits the ISA-mode var at
//! its TARGET, a flowing change point that paints forward to the next one. A
//! callee below the caller paints the caller's own bytes, so the caller's
//! successors must decode in the mode their region decoded in, not in what the
//! engine holds at the region start, and fresh and reused engines agree.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Cfg, CfgOptions, DecodeEngine};
use strider_target::SleighArch;

type Engine = DecodeEngine<BufMemReader<Vec<u8>>>;

fn engine(arch: SleighArch, bytes: Vec<u8>, base: u64) -> Engine {
    DecodeEngine::new(arch, BufMemReader::new(bytes, base)).expect("decode engine")
}

fn build(e: &mut Engine, entry: u64, size: u64) -> Cfg {
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(size),
        ..CfgOptions::default()
    };
    e.build_cfg(entry.into(), &opts).expect("build")
}

/// Every decoded instruction as `(address, machine length, opcodes)`, sorted.
fn decode(cfg: &Cfg) -> Vec<(u64, u32, Vec<rsleigh::Opcode>)> {
    let mut v: Vec<_> = cfg
        .regions()
        .flat_map(|r| r.insns.iter())
        .map(|i| (i.addr.machine(), i.len, i.insn.opcode))
        .fold(Vec::<(u64, u32, Vec<_>)>::new(), |mut acc, (a, l, op)| {
            match acc.last_mut() {
                Some(last) if last.0 == a => last.2.push(op),
                _ => acc.push((a, l, vec![op])),
            }
            acc
        });
    v.sort_by_key(|e| e.0);
    v
}

fn le_words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn put(buf: &mut [u8], at: usize, bytes: &[u8]) {
    buf[at..at + bytes.len()].copy_from_slice(bytes);
}

/// ARM at 0x2000 calls a Thumb `bx lr` at 0x1000:
/// `blx 0x1000; cmp r0,#0; beq 0x2010; mov r0,#1; bx lr`.
fn arm_calls_thumb_below() -> Vec<u8> {
    let mut b = vec![0u8; 0x1014];
    put(&mut b, 0, &0x4770u16.to_le_bytes());
    put(
        &mut b,
        0x1000,
        &le_words(&[
            0xfaff_fbfe,
            0xe350_0000,
            0x0a00_0000,
            0xe3a0_0001,
            0xe12f_ff1e,
        ]),
    );
    b
}

#[test]
fn an_arm_caller_of_a_lower_thumb_callee_decodes_its_successors_as_arm() {
    let mut fresh = engine(SleighArch::arm(), arm_calls_thumb_below(), 0x1000);
    let cold = decode(&build(&mut fresh, 0x2000, 0x14));
    assert!(
        cold.iter().all(|(_, len, _)| *len == 4),
        "every instruction of the ARM caller is 4 bytes: {cold:?}",
    );
    assert_eq!(cold.len(), 5, "{cold:?}");

    let mut reused = engine(SleighArch::arm(), arm_calls_thumb_below(), 0x1000);
    let _ = build(&mut reused, 0x1001, 2);
    assert_eq!(decode(&build(&mut reused, 0x2000, 0x14)), cold);
}

/// Thumb at 0x2000 calls an ARM `bx lr` at 0x1c00:
/// `blx 0x1c00; cmp r0,#0; beq 0x200c; movs r0,#1; nop; bx lr`, on an engine
/// whose last build was a Thumb one at 0x1800, so the entry pin writes no
/// change point of its own.
fn thumb_calls_arm_below() -> Vec<u8> {
    let mut b = vec![0u8; 0x810];
    put(&mut b, 0, &0x4770u16.to_le_bytes());
    put(&mut b, 0x400, &0xe12f_ff1eu32.to_le_bytes());
    let halves: [u16; 7] = [0xf7ff, 0xedfe, 0x2800, 0xd001, 0x2001, 0xbf00, 0x4770];
    let bytes: Vec<u8> = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
    put(&mut b, 0x800, &bytes);
    b
}

#[test]
fn a_thumb_caller_of_a_lower_arm_callee_decodes_its_successors_as_thumb() {
    let mut fresh = engine(SleighArch::arm(), thumb_calls_arm_below(), 0x1800);
    let cold = decode(&build(&mut fresh, 0x2001, 0x10));
    assert!(
        cold.iter().skip(1).all(|(_, len, _)| *len == 2),
        "past the blx every instruction is 2 bytes: {cold:?}",
    );

    let mut reused = engine(SleighArch::arm(), thumb_calls_arm_below(), 0x1800);
    let _ = build(&mut reused, 0x1801, 2);
    assert_eq!(decode(&build(&mut reused, 0x2001, 0x10)), cold);
}

/// MIPS32 BE at 0x2000: `jalx 0x1000; nop; jr ra; nop`, calling a MIPS16
/// `jr ra; nop` at 0x1000 (and another at 0x1800).
fn mips_jalx_below() -> Vec<u8> {
    let mut b = vec![0u8; 0x1010];
    put(&mut b, 0, &[0xe8, 0x20, 0x65, 0x00]);
    put(&mut b, 0x800, &[0xe8, 0x20, 0x65, 0x00]);
    let words: [u32; 4] = [0x7400_0400, 0, 0x03e0_0008, 0];
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    put(&mut b, 0x1000, &bytes);
    b
}

#[test]
fn a_mips32_jalx_to_a_lower_mips16_callee_decodes_its_caller_as_mips32() {
    let mut fresh = engine(SleighArch::mipsbe32(), mips_jalx_below(), 0x1000);
    let cold = decode(&build(&mut fresh, 0x2000, 0x10));
    assert_eq!(
        cold.iter()
            .map(|(a, len, _)| (*a, *len))
            .collect::<Vec<_>>(),
        [(0x2000, 8), (0x2008, 8)],
        "two MIPS32 branches, each with its slot",
    );

    let mut reused = engine(SleighArch::mipsbe32(), mips_jalx_below(), 0x1000);
    let _ = build(&mut reused, 0x1801, 4);
    assert_eq!(decode(&build(&mut reused, 0x2000, 0x10)), cold);
}

/// thumb/6.6-clang vmlinux `__bad_stack` (0x8010984c, 156 bytes): a Thumb
/// `bx pc` into an ARM body that ends `blx handle_bad_stack; nop`, with
/// `handle_bad_stack` at 0x80109682, below it.
const BAD_STACK: u64 = 0x8010_984c;
const BAD_STACK_WORDS: [u32; 39] = [
    0xbf004778, 0xee1dcf90, 0xe08cc00f, 0xe28cc884, 0xe28ccc99, 0xe59cc0cc, 0xe52cd004, 0xe1a0d00c,
    0xe49dc004, 0xe52dc008, 0xe92d4080, 0xe590c004, 0xe58dc00c, 0xe590c00c, 0xe24dd04c, 0xe08dd001,
    0xe04d1001, 0xe3110004, 0xe04d1001, 0xe04dd001, 0x124dd004, 0xe88d1fff, 0xe8900038, 0xe28d7034,
    0xe3e06000, 0xe28d204c, 0x12822004, 0xe58d3000, 0xe1a0300e, 0xe887007c, 0xee1d9f70, 0xee130f10,
    0xe58d0048, 0xe59d7034, 0xe5971008, 0xe58d1034, 0xe1a0000d, 0xfbffff66, 0xe320f000,
];

#[test]
fn the_nop_after_a_kernel_blx_to_a_lower_callee_stays_an_arm_nop() {
    // The ARM body, entered as its `$a` mapping symbol says.
    let entry = BAD_STACK + 4;
    let size = 152;
    let mut fresh = engine(SleighArch::arm(), le_words(&BAD_STACK_WORDS), BAD_STACK);
    let cold = build(&mut fresh, entry, size);
    let nop = cold
        .regions()
        .find(|r| r.start_addr.machine() == BAD_STACK + 0x98)
        .expect("a region at the nop");
    assert!(
        nop.insns.is_empty(),
        "the ARM nop lifts to no p-code, not a Thumb blx: {:?}",
        decode(&cold),
    );

    let mut reused = engine(SleighArch::arm(), le_words(&BAD_STACK_WORDS), BAD_STACK);
    let _ = build(&mut reused, BAD_STACK | 1, 4);
    assert_eq!(decode(&build(&mut reused, entry, size)), decode(&cold));
}
