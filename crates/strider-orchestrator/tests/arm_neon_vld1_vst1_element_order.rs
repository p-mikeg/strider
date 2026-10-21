//! `vld1` / `vst1` (multiple single elements) transfer each element on its
//! own, `Elem[D[d+r], e, esize] = MemU[address, ebytes]` and back (Arm ARM
//! DDI 0406C, VLD1 / VST1 (multiple single elements)), so with big-endian
//! data each element is byte-reversed in place and element 0 stays at the
//! lowest address. One 8-byte transfer is the same thing only for `.64`, or
//! for any element size with little-endian data.

mod common;

use std::collections::BTreeMap;

use common::Arch;
use strider_ir::node::{IntBinaryOp, NodeKind};
use strider_ir::{Function, IRViewer, IRWalker};
use strider_orchestrator::opt::ReadOnlyMemory;
use strider_target::Endianness;

const BASE: u64 = 0x1000;
/// Where the loads read from: read-only, so each folds to its bytes.
const DATA: u64 = 0x2000;
const DATA_BYTES: [u8; 16] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xf0, 0x0f,
];

struct Rom;

impl ReadOnlyMemory for Rom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let at = addr
            .checked_sub(DATA)
            .and_then(|a| usize::try_from(a).ok())
            .filter(|&a| a + buf.len() <= DATA_BYTES.len())
            .ok_or_else(|| anyhow::anyhow!("{addr:#x} is not read-only"))?;
        buf.copy_from_slice(&DATA_BYTES[at..at + buf.len()]);
        Ok(())
    }
}

fn analyze(arch: Arch, body: &[u32]) -> Function {
    // BE8 keeps instructions little-endian; only BE32 stores them big-endian.
    let code_order = if arch == Arch::ArmBe {
        Endianness::Big
    } else {
        Endianness::Little
    };
    let mut words = body.to_vec();
    words.push(0xe12f_ff1e); // bx lr
    let bytes = common::words_image(&words, code_order);
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, Some(Box::new(Rom)));
    strider
        .analyze(BASE, &cc, &Default::default(), None)
        .expect("analyze")
        .function
}

/// `d` as `(d[31:0], d[63:32])` after loading `esize`-bit elements from
/// `bytes`, each read in `big` or little-endian order.
fn d_from_elements(bytes: &[u8], esize: usize, big: bool) -> (u128, u128) {
    let eb = esize / 8;
    let mut d = 0u64;
    for (e, elem) in bytes[..8].chunks(eb).enumerate() {
        let mut le = elem.to_vec();
        if big {
            le.reverse();
        }
        le.resize(8, 0);
        let v = u64::from_le_bytes(le.try_into().expect("8 bytes"));
        d |= v << (e * esize);
    }
    (u128::from(d & 0xffff_ffff), u128::from(d >> 32))
}

/// `movw r0, #DATA; movt r0, #0`
const R0_IS_DATA: [u32; 2] = [0xe302_0000, 0xe340_0000];
/// `vmov r0, r1, d0` and `vmov r0, r1, d1`
const VMOV_FROM: [u32; 2] = [0xec51_0b10, 0xec51_0b11];

const LOADS: [(&str, u32, usize); 4] = [
    ("vld1.8 {d0}, [r0]", 0xf420_070f, 8),
    ("vld1.16 {d0}, [r0]", 0xf420_074f, 16),
    ("vld1.32 {d0}, [r0]", 0xf420_078f, 32),
    ("vld1.64 {d0}, [r0]", 0xf420_07cf, 64),
];

fn loads_wrong(arch: Arch, big: bool) -> Vec<String> {
    let mut wrong = Vec::new();
    for (name, insn, esize) in LOADS {
        let f = analyze(arch, &[R0_IS_DATA[0], R0_IS_DATA[1], insn, VMOV_FROM[0]]);
        let ret = common::returned(&f);
        let got = (f.int_const_u128(ret[0]), f.int_const_u128(ret[1]));
        let (lo, hi) = d_from_elements(&DATA_BYTES, esize, big);
        if got != (Some(lo), Some(hi)) {
            wrong.push(format!(
                "{arch:?} {name}: {got:x?}, want ({lo:#x}, {hi:#x})"
            ));
        }
    }
    wrong
}

/// The bytes the function's stores leave at `r0 + offset`, each store
/// written in `big` or little-endian order.
fn stored_bytes(f: &Function, big: bool) -> BTreeMap<i128, u8> {
    let r0 = |v| matches!(f.kind_of_value(v), NodeKind::InitialVar(_));
    let mut mem = BTreeMap::new();
    for n in f.walk() {
        if !matches!(f.node_kind(n), NodeKind::Store(_)) {
            continue;
        }
        let [_, addr, data] = f.node_inputs_exact::<3>(n).expect("a Store's inputs");
        let offset = match f.kind_of_value(addr) {
            _ if r0(addr) => 0,
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let [base, off] = f.producer_inputs_exact::<2>(addr).expect("an Add");
                assert!(r0(base), "a store off r0");
                f.int_const_i128(off).expect("a constant offset")
            }
            other => panic!("a store address {other:?}"),
        };
        let width = f.value_type(data).expect("typed").bit_width() / 8;
        let value = f.int_const_u128(data).expect("a constant stored value");
        let mut bytes = value.to_le_bytes()[..width].to_vec();
        if big {
            bytes.reverse();
        }
        for (i, b) in bytes.into_iter().enumerate() {
            mem.insert(offset + i128::try_from(i).expect("small"), b);
        }
    }
    mem
}

/// `movw r1, #0x2211; movt r1, #0x4433; movw r2, #0x6655; movt r2, #0x8877;
/// vmov d0, r1, r2; vmov d1, r2, r1`
const D0_D1: [u32; 6] = [
    0xe302_1211,
    0xe344_1433,
    0xe306_2655,
    0xe348_2877,
    0xec42_1b10,
    0xec41_2b11,
];
const D0: u64 = 0x8877_6655_4433_2211;
const D1: u64 = 0x4433_2211_8877_6655;

const STORES: [(&str, u32, usize); 4] = [
    ("vst1.8 {d0}, [r0]", 0xf400_070f, 8),
    ("vst1.16 {d0}, [r0]", 0xf400_074f, 16),
    ("vst1.32 {d0}, [r0]", 0xf400_078f, 32),
    ("vst1.64 {d0}, [r0]", 0xf400_07cf, 64),
];

/// The bytes `esize`-bit elements of `regs` occupy in memory.
fn element_bytes(regs: &[u64], esize: usize, big: bool) -> Vec<u8> {
    let eb = esize / 8;
    let mut out = Vec::new();
    for d in regs {
        for elem in d.to_le_bytes().chunks(eb) {
            if big {
                out.extend(elem.iter().rev());
            } else {
                out.extend(elem);
            }
        }
    }
    out
}

fn stores_wrong(arch: Arch, big: bool) -> Vec<String> {
    let mut wrong = Vec::new();
    for (name, insn, esize) in STORES {
        let mut body = D0_D1.to_vec();
        body.push(insn);
        let f = analyze(arch, &body);
        let got: Vec<u8> = stored_bytes(&f, big).into_values().collect();
        let want = element_bytes(&[D0], esize, big);
        if got != want {
            wrong.push(format!("{arch:?} {name}: {got:02x?}, want {want:02x?}"));
        }
    }
    wrong
}

#[test]
fn big_endian_data_transfers_each_element_on_its_own() {
    let mut wrong = Vec::new();
    for arch in [Arch::ArmBe8, Arch::ArmBe] {
        wrong.extend(loads_wrong(arch, true));
        wrong.extend(stores_wrong(arch, true));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn little_endian_data_is_unchanged() {
    let mut wrong = loads_wrong(Arch::Arm, false);
    wrong.extend(stores_wrong(Arch::Arm, false));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each register of a list is the next 8 bytes, transferred by element.
#[test]
fn a_register_list_transfers_each_register_by_element() {
    let mut wrong = Vec::new();
    for (arch, big) in [
        (Arch::ArmBe8, true),
        (Arch::ArmBe, true),
        (Arch::Arm, false),
    ] {
        // vld1.16 {d0, d1}, [r0]; vmov r0, r1, d1
        let f = analyze(
            arch,
            &[R0_IS_DATA[0], R0_IS_DATA[1], 0xf420_0a4f, VMOV_FROM[1]],
        );
        let ret = common::returned(&f);
        let got = (f.int_const_u128(ret[0]), f.int_const_u128(ret[1]));
        let (lo, hi) = d_from_elements(&DATA_BYTES[8..], 16, big);
        if got != (Some(lo), Some(hi)) {
            wrong.push(format!(
                "{arch:?} vld1.16 {{d0, d1}}: {got:x?}, want ({lo:#x}, {hi:#x})"
            ));
        }

        // vst1.32 {d0, d1}, [r0]
        let mut body = D0_D1.to_vec();
        body.push(0xf400_0a8f);
        let f = analyze(arch, &body);
        let got: Vec<u8> = stored_bytes(&f, big).into_values().collect();
        let want = element_bytes(&[D0, D1], 32, big);
        if got != want {
            wrong.push(format!(
                "{arch:?} vst1.32 {{d0, d1}}: {got:02x?}, want {want:02x?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
