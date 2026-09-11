//! Hostile or malformed bytes must leave the process standing, answering with
//! a `Cfg` or a clean error. These inputs each used to take the process down
//! or panic out of a public entry point.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, CfgOptions};
use strider_target::SleighArch;

fn hex(s: &[u8]) -> Vec<u8> {
    s.chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

/// Builds and reports whether it succeeded: returning at all is half of every
/// assertion here, a crash or a panic being what these inputs used to produce.
fn build_succeeds(arch: &SleighArch, bytes: Vec<u8>, start: u64, opts: &CfgOptions) -> bool {
    let reader = BufMemReader::new(bytes, start);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create Sleigh");
    Builder::for_arch(arch, &mut sleigh, start, opts)
        .build()
        .is_ok()
}

/// Bytes Sleigh rejects, so the build must answer with a clean error.
fn build_is_an_error(arch: &SleighArch, bytes: Vec<u8>, start: u64, opts: &CfgOptions) {
    assert!(
        !build_succeeds(arch, bytes, start, opts),
        "bytes that do not decode must not build a Cfg"
    );
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
    build_is_an_error(&SleighArch::mipsbe32(), be.clone(), 0, &opts);
    build_is_an_error(&SleighArch::mipsbe64(), be, 0, &opts);
    build_is_an_error(&SleighArch::mipsle32(), le.clone(), 0, &opts);
    build_is_an_error(&SleighArch::mipsle64(), le, 0, &opts);
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
    build_is_an_error(
        &SleighArch::aarch64(),
        hex(b"5ddc2439c5f99a4f3a6f11f5abfc6d71"),
        0,
        &opts,
    );
    build_is_an_error(
        &SleighArch::aarch64be(),
        hex(b"c3f8804f206502ccdccff836a472b5d5"),
        0,
        &opts,
    );
}

/// The lane operand of a by-element dot product used to reach the pcodeop
/// itself, and its subtable exports nothing, so on a reused `ParserContext` it
/// carried whatever register the previous instruction left in the slot. The
/// constructors now read the lane with `SIMD_PIECE`, so the operand is the one
/// the encoding names whatever ran before it.
#[test]
fn a_by_element_dot_product_reads_the_register_its_encoding_names() {
    let arch = SleighArch::aarch64();
    // ldr q7,[x0] ; ldr q13,[x1] ; usdot v0.4s,v1.16b,v2.4b[0] ; ret
    let dirtied = hex(b"0700c03d2d00c03d20f0824fc0035fd6");
    let alone = hex(b"20f0824fc0035fd6");

    let reg_inputs = |bytes: Vec<u8>, usdot_at: u64| {
        let reader = BufMemReader::new(bytes, 0);
        let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        // Decode from the top so the dirtied case really does reuse the
        // context the two loads left behind.
        let mut addr = 0;
        while addr < usdot_at {
            addr += sleigh
                .lift_one(addr)
                .expect("decode leading insn")
                .machine_insn_len as u64;
        }
        let lifted = sleigh.lift_one(usdot_at).expect("the dot product decodes");
        let regs: Vec<rsleigh::Vn> = lifted
            .insns
            .iter()
            .flat_map(|insn| insn.inputs.iter().copied())
            .filter(|vn| vn.addr_space == rsleigh::VnSpace::REGISTER)
            .collect();
        (regs, sleigh.regs().expect("regs").clone())
    };

    let (after_loads, regs) = reg_inputs(dirtied, 8);
    let (on_its_own, _) = reg_inputs(alone, 0);
    assert_eq!(
        after_loads, on_its_own,
        "the operand comes from the encoding, so what ran before cannot change it",
    );

    let off = |name: &str| {
        regs.name_to_vn(name)
            .expect("register must resolve")
            .addr_off
    };
    let reads = |name: &str| after_loads.iter().any(|vn| vn.addr_off == off(name));
    assert!(reads("q2"), "v2 is the encoded lane source");
    for stale in ["q7", "q13"] {
        assert!(
            !reads(stale),
            "{stale} is only what the loads left in the slot"
        );
    }
}

#[test]
fn a_region_at_the_top_of_the_address_space_does_not_panic() {
    // A stub region seated at an address whose span overflows `u64` gave
    // `BTreeMap::range` two equal excluded bounds, which panics.
    let opts = CfgOptions {
        fn_max_size: Some(0x10),
        ..CfgOptions::default()
    };
    let _ = build_succeeds(
        &SleighArch::x86_64(),
        vec![0x00, 0x00, 0x00, 0x00, 0x00, 0x76, 0x00, 0x76, 0xf6],
        0,
        &opts,
    );
}

#[test]
fn a_region_based_at_the_top_of_the_address_space_does_not_panic() {
    // The reader's end offset is `base + len`: seating the buffer so that sum
    // passes 2^64 overflowed it inside the read callback Sleigh calls across
    // the FFI boundary, where a panic cannot unwind and aborts the process.
    for start in [u64::MAX, u64::MAX - 1, u64::MAX - 4] {
        let _ = build_succeeds(
            &SleighArch::x86_64(),
            vec![0xc3, 0x48, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00],
            start,
            &CfgOptions::default(),
        );
    }
}
