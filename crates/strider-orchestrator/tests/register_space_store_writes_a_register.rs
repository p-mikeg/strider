//! A p-code STORE into the REGISTER space writes a register, not memory.
//!
//! A sla addresses a register through LOAD / STORE when an instruction field
//! picks it rather than naming it outright: ARM's `vld1.N {dX[i]}, [addr]`
//! writes lane `i` of `dX` that way, with the address built from constants the
//! decoder substituted. Lifting it as an opaque memory store left the register
//! never written and no phi placed for it, and `is_complete()` stayed true --
//! the value simply vanished, with nothing reported.
//!
//! `sub sp,#16; str r0,[sp]; vld1.32 {d0[0]},[sp]; vmov.32 r0,d0[0]; add
//! sp,#16; bx lr` is the round trip: it must return the `r0` it was given.

use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;

const ROUND_TRIP: [u8; 24] = [
    0x10, 0xd0, 0x4d, 0xe2, // sub sp, sp, #16
    0x00, 0x00, 0x8d, 0xe5, // str r0, [sp]
    0x0f, 0x08, 0xad, 0xf4, // vld1.32 {d0[0]}, [sp]
    0x10, 0x0b, 0x10, 0xee, // vmov.32 r0, d0[0]
    0x10, 0xd0, 0x8d, 0xe2, // add sp, sp, #16
    0x1e, 0xff, 0x2f, 0xe1, // bx lr
];

#[test]
fn a_neon_lane_round_trip_returns_its_argument() {
    let arch = strider_target::SleighArch::arm();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(ROUND_TRIP.to_vec(), BASE),
    )
    .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::arm_aapcs()
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

    // The lane write is a register write now, so the only Store left is the
    // `str r0,[sp]` into RAM.
    for n in f.graph().all_node_ids() {
        if let NodeKind::Store(space) = f.node_kind(n) {
            assert_eq!(
                *space,
                rsleigh::VnSpace::RAM,
                "a register-space store survived as memory"
            );
        }
    }

    // r0 in, r0 out: with the lane write missing the read saw an unwritten
    // register, and the argument never reached the return.
    let r0 = regs.name_to_vn("r0").expect("r0");
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("Return");
    let ret_inputs: Vec<_> = f.node_inputs(ret).into_iter().collect();
    assert!(ret_inputs.len() >= 3, "Return must carry a value");
    let val_node = f.producer(ret_inputs[2]);
    let NodeKind::InitialVar(vn_id) = *f.node_kind(val_node) else {
        panic!(
            "the returned value is {:?}, not the incoming r0",
            f.node_kind(val_node)
        );
    };
    assert_eq!(
        f.initial_vn(vn_id),
        r0,
        "the round trip must return the r0 it was given"
    );
}
