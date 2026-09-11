//! Shared end-to-end pipeline runners + placeholder-target finders for the
//! IR-level fixture builders.

#![allow(dead_code)]

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::MachineInsnAddr;
use strider_ir::node::NodeKind;
use strider_ir::{Function, IRViewer, IRWalker};
use strider_orchestrator::Lifter;
use strider_target::{CallingConvention, SleighArch};

/// Walk every reachable `IndirectBranch` node and return the value-
/// input (slot 2) of the unique placeholder.  Returns `None` if none
/// exists; panics if more than one is found (every fixture in this
/// module has exactly one indirect branch).
///
/// The placeholder `IndirectBranch` has exactly 3 inputs:
/// `[control, memory, target_value]`.
pub(crate) fn target_value_input(function: &Function) -> Option<strider_ir::Value> {
    let mut found: Option<strider_ir::Value> = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
        assert!(
            inputs.len() == 3,
            "IndirectBranch placeholder must have exactly 3 inputs; got {}",
            inputs.len(),
        );
        assert!(
            found.is_none(),
            "fixture must have exactly one IndirectBranch placeholder; found a second",
        );
        found = Some(inputs[2]);
    }
    found
}

/// Lift a hand-assembled byte sequence under SystemV-x86_64 and run the
/// full optimiser pipeline.  Returns the resulting graph plus the
/// (single) IR-level placeholder target's `ValueId` and the
/// convention's link-register VN (always `None` on x86_64, which
/// pushes return addresses on the stack instead).
///
/// Panics if the synthetic CFG produces zero or multiple
/// `UnresolvedIndirectBranch` placeholders; every fixture in this
/// module has exactly one indirect branch.
pub(crate) fn run_pipeline_x86_64(
    bytes: Vec<u8>,
) -> (Function, strider_ir::Value, Option<rsleigh::Vn>) {
    let base = 0x1000u64;
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh =
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create x86_64 sleigh");

    let mut strider = Lifter::new(arch, sleigh).expect("Lifter::new");
    let cc = CallingConvention::x86_64_systemv()
        .build(strider.sleigh_regs())
        .expect("build cc");
    let lr_vn = cc.link_register_vn;
    let cfg = strider
        .build_cfg(
            MachineInsnAddr::from(base),
            &strider_cfg::CfgOptions::default(),
            &Default::default(),
        )
        .expect("cfg build");
    let outcome = strider.build_ir(&cfg, cc).expect("build_ir");
    let mut function = outcome.function;

    // ConstantFold collapses `mov rax, K; jmp *rax` to IntConst(K);
    // PhiCollapse simplifies the trivial Return shape so the classifier
    // sees the producer-shape it expects.
    let p = strider_orchestrator::opt::default_pipeline();
    p.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("optimizer pipeline");

    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "fixture must have exactly one IR-level placeholder",
    );
    // Resolve the *current* target after the optimiser ran: the original
    // recorded ValueId may be orphaned if any pass `replace_all_uses`-rewrote
    // the placeholder's input slot (e.g. ConstantFold folding an
    // IntBinaryOp into an IntConst).
    let target = target_value_input(&function)
        .expect("fixture must have one IndirectBranch placeholder after optimisation");
    (function, target, lr_vn)
}
