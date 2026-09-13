//! A ppc64 ELFv1 image, where an `STT_FUNC` symbol names an `.opd` function
//! descriptor, lifts at the entry the descriptor holds.
//!
//! `ELFV1_IMAGE` is `powerpc64-linux-gnu-gcc` 11.4.0 output for
//!
//! ```c
//! long add7(long a) { return a + 7; }
//! long answer(void) { return 42; }
//! ```
//!
//! built with `-mabi=elfv1 -O2 -nostdlib -static -fno-pic -no-pie
//! -fno-asynchronous-unwind-tables -fno-unwind-tables -Wl,--build-id=none
//! -Wl,-z,max-page-size=0x1000 -Wl,-z,norelro -Wl,-N -Wl,-e,answer`, then
//! `objcopy -R .comment` (sha256 `c6c4f6bd...e7b7c57a`, 1016 bytes).
//! `readelf -h` reports `Flags: 0x1, abiv1`, and `objdump -d`:
//!
//! ```text
//! 10000080 <.add7>:    38 63 00 07  addi r3,r3,7
//! 10000084:            4e 80 00 20  blr
//! 100000a0 <.answer>:  38 60 00 2a  li   r3,42
//! 100000a4:            4e 80 00 20  blr
//! 100000b8 <add7>:     .opd {0x10000080, toc 0x10008000, env 0}
//! 100000d0 <answer>:   .opd {0x100000a0, toc 0x10008000, env 0}
//! ```

use object::{Object as _, ObjectSection as _, ObjectSymbol as _};
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IntBinaryOp};
use strider_reader::{ElfFileMemReader, OwnedElf};
use strider_target::{CallingConvention, SleighArch};

mod common;

const ELFV1_IMAGE_HEX: &str = "\
    7f454c46020201000000000000000000000200150000000100000000100000d0\
    0000000000000040000000000000023800000001004000380001004000070006\
    0000000100000007000000000000008000000000100000800000000010000080\
    0000000000000068000000000000006800000000000000100000000000000000\
    386300074e800020000000000000000000000000600000006000000060420000\
    3860002a4e800020000000000000000000000000000000000000000010000080\
    0000000010008000000000000000000000000000100000a00000000010008000\
    0000000000000000000000000000000000000000000000000000000000000000\
    0000000003000001000000001000008000000000000000000000000003000002\
    00000000100000b40000000000000000000000000300000300000000100000b8\
    0000000000000000000000010400fff100000000000000000000000000000000\
    000000061200000300000000100000b800000000000000140000000b10000003\
    00000000100000e80000000000000000000000171200000300000000100000d0\
    00000000000000140000001e1000000300000000100000e80000000000000000\
    000000251000000300000000100000e800000000000000000076312e63006164\
    6437005f5f6273735f737461727400616e73776572005f6564617461005f656e\
    6400002e73796d746162002e737472746162002e7368737472746162002e7465\
    7874002e65685f6672616d65002e6f7064000000000000000000000000000000\
    0000000000000000000000000000000000000000000000000000000000000000\
    0000000000000000000000000000000000000000000000000000001b00000001\
    0000000000000007000000001000008000000000000000800000000000000034\
    0000000000000000000000000000001000000000000000000000002100000001\
    000000000000000200000000100000b400000000000000b40000000000000000\
    0000000000000000000000000000000400000000000000000000002b00000001\
    000000000000000300000000100000b800000000000000b80000000000000030\
    0000000000000000000000000000000800000000000000000000000100000002\
    0000000000000000000000000000000000000000000000e800000000000000f0\
    0000000500000005000000000000000800000000000000180000000900000003\
    0000000000000000000000000000000000000000000001d8000000000000002a\
    0000000000000000000000000000000100000000000000000000001100000003\
    0000000000000000000000000000000000000000000002020000000000000030\
    000000000000000000000000000000010000000000000000";

const ADD7_ENTRY: u64 = 0x1000_0080;
const ANSWER_ENTRY: u64 = 0x1000_00a0;

fn image() -> OwnedElf {
    let hex = ELFV1_IMAGE_HEX.as_bytes();
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(std::str::from_utf8(&hex[i..i + 2]).unwrap(), 16).unwrap())
        .collect::<Vec<u8>>();
    assert_eq!(bytes.len(), 1016);
    OwnedElf::parse(bytes).expect("parse")
}

/// `(st_value, name of the section st_value lies in)`.
fn symbol(elf: &OwnedElf, name: &str) -> (u64, String) {
    let obj = elf.checked_file().expect("file");
    let addr = obj.symbol_by_name(name).expect("symbol").address();
    let section = obj
        .sections()
        .find(|s| (s.address()..s.address() + s.size()).contains(&addr))
        .expect("a section holds the symbol");
    (addr, section.name().expect("name").to_owned())
}

/// The lifted and optimised function at `entry` under `powerpc64_elf_v1`.
fn analyze(elf: &OwnedElf, entry: u64) -> strider_ir::Function {
    let obj = elf.checked_file().expect("file");
    let arch = SleighArch::ppc64be();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        ElfFileMemReader::from_object(&obj).expect("mem"),
    )
    .expect("sleigh");
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(ElfFileMemReader::from_object(&obj).expect("rom"));
    let mut strider = strider_orchestrator::Strider::new(arch, sleigh, Some(rom)).expect("strider");
    let cc = CallingConvention::powerpc64_elf_v1()
        .build(strider.sleigh_regs())
        .expect("cc");
    strider
        .analyze(entry, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze")
        .function
}

#[test]
fn a_function_symbol_names_its_opd_descriptor() {
    let elf = image();
    assert_eq!(symbol(&elf, "add7"), (0x1000_00b8, ".opd".to_owned()));
    assert_eq!(symbol(&elf, "answer"), (0x1000_00d0, ".opd".to_owned()));
}

#[test]
fn the_descriptor_resolves_to_the_text_entry() {
    let elf = image();
    for (name, entry) in [("add7", ADD7_ENTRY), ("answer", ANSWER_ENTRY)] {
        let (descriptor, _) = symbol(&elf, name);
        assert_eq!(
            elf.function_entry(descriptor).expect("entry"),
            entry,
            "{name}"
        );
    }
    assert_eq!(
        elf.function_entry(ANSWER_ENTRY).expect("entry"),
        ANSWER_ENTRY,
        "a code address passes through"
    );
}

#[test]
fn answer_lifts_at_its_entry_and_returns_42() {
    let elf = image();
    let (descriptor, _) = symbol(&elf, "answer");
    let f = analyze(&elf, elf.function_entry(descriptor).expect("entry"));
    let r3 = common::returned(&f)[0];
    assert_eq!(f.int_const_u128(r3), Some(42));
}

#[test]
fn add7_lifts_at_its_entry_and_returns_r3_plus_7() {
    let elf = image();
    let (descriptor, _) = symbol(&elf, "add7");
    let f = analyze(&elf, elf.function_entry(descriptor).expect("entry"));
    let r3 = common::returned(&f)[0];
    let add = f.producer(r3);
    assert!(
        matches!(f.node_kind(add), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
        "r3 is {:?}",
        f.node_kind(add)
    );
    let operands: Vec<_> = f.node_inputs(add).into_iter().collect();
    assert!(
        operands.iter().any(|&v| f.int_const_u128(v) == Some(7)),
        "an operand is 7"
    );
    assert!(
        operands
            .iter()
            .any(|&v| matches!(f.node_kind(f.producer(v)), NodeKind::InitialVar(_))),
        "the other is the incoming r3"
    );
}
