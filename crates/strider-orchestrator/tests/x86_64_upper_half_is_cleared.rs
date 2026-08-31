//! A 32-bit destination clears the upper half of its 64-bit container.
//!
//! Intel SDM Vol. 2B: for `RDTSC` / `RDPMC`, "in 64-bit mode, the high-order 32
//! bits of each of RAX and RDX are cleared". The sla reaches that through
//! `check_EAX_dest` / `check_EDX_dest`; three constructors wrote the halves
//! from a temporary without them, so the container kept the CALLER's bits and
//! the result also carried a false dependency on the incoming register.

use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRViewer;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x10000;
/// The mask a read-modify-write of the low half leaves behind.
const STALE_UPPER_HALF: u128 = 0xFFFF_FFFF_0000_0000;

fn keeps_the_callers_high_half(bytes: Vec<u8>) -> bool {
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, BASE),
    )
    .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    let f = &result.function;
    f.graph()
        .all_value_ids()
        .any(|v| f.int_const_u128(v).is_some_and(|c| c == STALE_UPPER_HALF))
}

#[test]
fn reading_a_counter_into_eax_edx_clears_the_upper_halves() {
    for (name, bytes) in [
        ("rdtsc", vec![0x0f, 0x31, 0xc3]),
        ("rdpmc", vec![0x0f, 0x33, 0xc3]),
        ("xgetbv", vec![0x0f, 0x01, 0xd0, 0xc3]),
    ] {
        assert!(
            !keeps_the_callers_high_half(bytes),
            "{name} kept the caller's high 32 bits in RAX/RDX",
        );
    }
}
