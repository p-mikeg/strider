//! The AArch64 round-to-integral family is not a float-to-integer conversion.
//!
//! `FRINTP/M/A/N/Z/X/I` round a float to an integral VALUE, keeping the float
//! type. Sleigh's `trunc()` is p-code `FLOAT_TRUNC`, which converts a float to
//! an INTEGER, and one constructor per operand shape carried it for all seven
//! rounding modes, so `ceil(2.5)` answered `2` rather than `3.0` -- the wrong
//! domain as well as the wrong direction. Upstream marks these `--status fail
//! --comment "nofpround"`.
//!
//! p-code has no primitive for four of the seven modes (ties-to-even, toward
//! zero, and the two that read the runtime rounding mode), so the family lifts
//! opaquely rather than claiming a rounding it cannot express. `NEON_frint` is
//! pure, so the function still lifts and only the rounded value is unknown.

use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x10000;

/// `frintp d0, d0` then `ret`: what gcc -O2 emits for `__builtin_ceil`.
const FRINTP_RET: [u8; 8] = [0x00, 0xc0, 0x64, 0x1e, 0xc0, 0x03, 0x5f, 0xd6];

#[test]
fn rounding_to_integral_does_not_lift_as_a_conversion_to_integer() {
    let arch = strider_target::SleighArch::aarch64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(FRINTP_RET.to_vec(), BASE),
    )
    .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::aarch64_aapcs64()
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
        .expect("frintp must still lift");
    let f = &result.function;

    let float_to_int = f
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::FloatToInt))
        .count();
    assert_eq!(
        float_to_int, 0,
        "frintp lifted as a float-to-integer conversion"
    );
    let call_other = f
        .graph()
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::CallOther { .. }))
        .count();
    assert!(call_other > 0, "frintp must reach the IR as an opaque op");
}
