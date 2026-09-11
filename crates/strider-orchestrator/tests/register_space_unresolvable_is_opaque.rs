//! A REGISTER-space access whose address names no register lifts opaquely
//! rather than failing the function.
//!
//! The sla builds such an address from constants when an instruction field
//! picks the register, and it folds only where the p-code is straight line.
//! ARM's multi-structure `VLD2/3/4` and `VST2/3/4` carry an INTRA-INSTRUCTION
//! loop whose register pointer is loop-carried, so the CFG splits the one
//! machine instruction into several regions and the pointer's definition lands
//! in a different region from its use. The per-region
//! constant folder cannot see across that, and a loop-carried pointer has no
//! single constant value to see anyway.
//!
//! Failing the whole function there lost every de-interleaving NEON function,
//! which the pre-0.2.0 lifter had handled (as a wrong memory store, but
//! handled). The opaque path keeps the function analysable and says nothing it
//! cannot back up.

use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;

fn lift(arch: strider_target::SleighArch, code: Vec<u8>, entry: u64) -> strider_ir::Function {
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), BufMemReader::new(code, BASE))
        .expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    strider
        .analyze(
            entry,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("a multi-structure NEON access must not fail the function")
        .function
}

/// Every `VLD2/3/4` and `VST2/3/4` multiple-structures form, at each element
/// size. `VLD1`/`VST1` are absent on purpose: Sleigh fully unrolls those with
/// direct register operands, so they never reach the register space at all.
#[test]
fn a_loop_carried_register_pointer_lifts_instead_of_failing() {
    let cases: [(&str, u32); 8] = [
        ("vld2.32 {d0,d1},[r0]", 0xF420088F),
        ("vld3.32 {d0,d1,d2},[r0]", 0xF420048F),
        ("vld4.32 {d0-d3},[r0]", 0xF420008F),
        ("vst2.32 {d0,d1},[r0]", 0xF400088F),
        ("vst3.32 {d0,d1,d2},[r0]", 0xF400048F),
        ("vst4.32 {d0-d3},[r0]", 0xF400008F),
        ("vld2.8 {d0,d1},[r0]", 0xF420080F),
        ("vld2.16 {d0,d1},[r0]", 0xF420084F),
    ];
    for (name, encoding) in cases {
        let mut code = encoding.to_le_bytes().to_vec();
        code.extend_from_slice(&0xE12F_FF1Eu32.to_le_bytes()); // bx lr
        let f = lift(strider_target::SleighArch::arm(), code, BASE);
        assert!(
            f.graph().all_node_ids().count() > 0,
            "{name} lifted to an empty graph"
        );
    }
}

/// The opaque store clobbers the register file, so a register read after it
/// cannot still be the function's entry value.
#[test]
fn an_opaque_register_store_clobbers_the_register_it_may_have_written() {
    // vld2.32 {d0,d1},[r0] ; vmov.32 r0,d0[0] ; bx lr
    let mut code = 0xF420_088Fu32.to_le_bytes().to_vec();
    code.extend_from_slice(&0xEE10_0B10u32.to_le_bytes());
    code.extend_from_slice(&0xE12F_FF1Eu32.to_le_bytes());
    let f = lift(strider_target::SleighArch::arm(), code, BASE);

    let ret = f
        .graph()
        .all_node_ids()
        .find(|n| matches!(f.node_kind(*n), NodeKind::Return))
        .expect("Return");
    // `Return` is `[CTRL, MEM] + RET*`, so the returned values start at 2.
    let inputs: Vec<_> = f.node_inputs(ret).into_iter().collect();
    let ret_val = *inputs.get(2).expect("Return carries a value slot");
    assert!(
        !matches!(f.node_kind(f.producer(ret_val)), NodeKind::InitialVar(_)),
        "d0 was read back as its entry value across an opaque write to it"
    );
}

/// The REGISTER space is a memory space of its own, so the opaque store and a
/// later opaque load at the same address are connected. Without that, a
/// re-read of the slot would be a second, unrelated unknown.
#[test]
fn an_opaque_register_access_stays_in_the_register_space() {
    let mut code = 0xF420_088Fu32.to_le_bytes().to_vec();
    code.extend_from_slice(&0xE12F_FF1Eu32.to_le_bytes());
    let f = lift(strider_target::SleighArch::arm(), code, BASE);

    let register_space_ops = f
        .graph()
        .all_node_ids()
        .filter(|n| {
            matches!(
                f.node_kind(*n),
                NodeKind::Store(rsleigh::VnSpace::REGISTER)
                    | NodeKind::Load(rsleigh::VnSpace::REGISTER)
            )
        })
        .count();
    assert!(
        register_space_ops > 0,
        "the unresolvable access left no register-space node behind"
    );
    for n in f.graph().all_node_ids() {
        if let NodeKind::Store(space) = f.node_kind(n) {
            assert_ne!(
                *space,
                rsleigh::VnSpace::RAM,
                "a register access leaked into RAM"
            );
        }
    }
}
