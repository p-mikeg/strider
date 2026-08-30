//! Hostile or malformed bytes must come back as an error. These inputs each
//! used to take the process down or panic out of a public entry point.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, CfgOptions};
use strider_target::SleighArch;

fn hex(s: &[u8]) -> Vec<u8> {
    s.chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

/// Builds and discards the outcome: `Ok` and `Err` are both acceptable, a
/// crash or a panic is not.
fn build_is_survivable(arch: &SleighArch, bytes: Vec<u8>, start: u64, opts: &CfgOptions) {
    let reader = BufMemReader::new(bytes, start);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
    let _ = Builder::for_arch(arch, &mut sleigh, start, opts).build();
}

#[test]
fn a_malformed_instruction_in_a_mips_delay_slot_is_an_error() {
    // `bne` whose delay slot holds a COP1 word with a reserved format field.
    // Building the delay slot throws, and the handler that renders the message
    // used to disassemble through a walker owned by an unwound stack frame,
    // writing tens of kilobytes past it.
    let opts = CfgOptions {
        fn_max_size: Some(0x100),
        ..CfgOptions::default()
    };
    let be = vec![0x14, 0x00, 0x00, 0x00, 0x46, 0xc9, 0x00, 0xac];
    let le = vec![0x00, 0x00, 0x00, 0x14, 0xac, 0x00, 0xc9, 0x46];
    build_is_survivable(&SleighArch::mipsbe32(), be.clone(), 0, &opts);
    build_is_survivable(&SleighArch::mipsbe64(), be, 0, &opts);
    build_is_survivable(&SleighArch::mipsle32(), le.clone(), 0, &opts);
    build_is_survivable(&SleighArch::mipsle64(), le, 0, &opts);
}

#[test]
fn an_unparsed_aarch64_operand_is_an_error() {
    // An AdvSIMD encoding whose operand the parse allocates but never builds
    // answers with a null address space, which `generateLocation` dereferenced
    // while emitting p-code.
    let opts = CfgOptions {
        fn_max_size: Some(0x40),
        ..CfgOptions::default()
    };
    build_is_survivable(
        &SleighArch::aarch64(),
        hex(b"5ddc2439c5f99a4f3a6f11f5abfc6d71"),
        0,
        &opts,
    );
    build_is_survivable(
        &SleighArch::aarch64be(),
        hex(b"c3f8804f206502ccdccff836a472b5d5"),
        0,
        &opts,
    );
}

/// A constructor that exports no result leaves its handle untouched, so on a
/// reused `ParserContext` the operand carries whatever the previous instruction
/// left there. `usdot` / `bfdot` by element hit this: alone they fail, but
/// after any instruction that dirties the slot they lifted with a register
/// taken from that instruction. The failure has to be the same either way.
#[test]
fn a_by_element_dot_product_does_not_lift_a_stale_operand() {
    let opts = CfgOptions {
        fn_max_size: Some(0x40),
        ..CfgOptions::default()
    };
    let arch = SleighArch::aarch64();
    // ldr q7,[x0] ; ldr q13,[x1] ; usdot v0.4s,v1.16b,v2.4b[0] ; ret
    let dirtied = hex(b"0700c03d2d00c03d20f0824fc0035fd6");
    let alone = hex(b"20f0824fc0035fd6");
    let reader = BufMemReader::new(dirtied, 0);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    assert!(
        Builder::for_arch(&arch, &mut sleigh, 0, &opts)
            .build()
            .is_err(),
        "the by-element operand is unresolved, so it must fail here exactly as \
         it does with no instruction in front of it",
    );
    build_is_survivable(&arch, alone, 0, &opts);
}

#[test]
fn a_region_at_the_top_of_the_address_space_is_an_error() {
    // A stub region seated at an address whose span overflows `u64` gave
    // `BTreeMap::range` two equal excluded bounds, which panics.
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    build_is_survivable(
        &SleighArch::x86_64(),
        vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x76, 0x00, 0x76, 0xf6],
        0,
        &opts,
    );
}
