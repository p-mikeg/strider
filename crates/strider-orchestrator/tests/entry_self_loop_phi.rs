//! Strider's CFG does not normalize the entry to be predecessor-free: a branch
//! back to the entry address is an edge into the entry region (pinned by
//! `strider-cfg`'s `jump_to_entry_address_forms_single_region_self_loop`), so
//! `while (1)`, a tail-recursive jump to the function start, and any indirect
//! branch resolved back to the entry all reach the lifter as a loop whose
//! header IS the entry.
//!
//! `dominance_frontiers` must keep the root: `immediate_dominator() == None`
//! marks both the root and the unreachable nodes, and skipping it leaves
//! `phi_placement` with nothing for the entry region.  `MemPhi` is placed
//! unconditionally, so memory stays correct while values lose their
//! loop-carried dependence:
//!
//! ```text
//! got       eax = Add(InitialVar(eax), 1)
//! expected  eax = Phi(InitialVar(eax), Add(phi, 1))
//! ```

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};
use strider_target::{CallingConvention, SleighArch};

/// ```text
/// 0x1000: 83 C0 01   add eax, 1     <- entry, and the loop header
/// 0x1003: FF C9      dec ecx
/// 0x1005: 75 F9      jne 0x1000     <- back-edge INTO the entry region
/// 0x1007: C3         ret
/// ```
const BASE: u64 = 0x1000;
const FN_SIZE: u64 = 8;

fn synthetic_bytes() -> Vec<u8> {
    vec![0x83, 0xC0, 0x01, 0xFF, 0xC9, 0x75, 0xF9, 0xC3]
}

fn analyze() -> strider_ir::Function {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(synthetic_bytes(), BASE);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");
    let regs = sleigh.regs().expect("regs");
    let cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let lift_opts = LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(FN_SIZE),
            ..Default::default()
        },
        ..LiftOptions::default()
    };
    let mut strider = Strider::new(arch, sleigh, None).unwrap();
    strider
        .analyze(BASE, &cc, &lift_opts, &OptOptions::default(), None)
        .expect("self-loop at the entry must lift")
        .function
}

#[test]
fn entry_region_self_loop_gets_a_value_phi() {
    let function = analyze();

    let phis = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Phi))
        .count();
    let initial_vars = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::InitialVar(_)))
        .count();

    assert!(
        initial_vars > 0,
        "sanity: the function reads incoming registers"
    );
    // The entry region is the only join in this function, so a phi anywhere
    // means the back-edge was reconciled.
    assert!(
        phis > 0,
        "the entry region is a loop header (back-edge from `jne`), so its \
         loop-carried registers need phis; found none, which means the lifted \
         IR reads the pre-loop value on every iteration"
    );
}
