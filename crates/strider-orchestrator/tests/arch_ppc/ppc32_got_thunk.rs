//! ppc32 `-fpic -mbss-plt` finds its GOT with `bl _GLOBAL_OFFSET_TABLE_-4;
//! mflr r30`: the word before the GOT is `blrl`, which returns to the caller
//! and leaves LR at the GOT. A per-address convention without LR in its
//! callee-saved registers clobbers LR at that call, so `mflr` reads the
//! callee's value rather than the `bl`'s own return address.

use strider_ir::IRViewer as _;
use strider_target::{BuiltCallingConvention, CallingConvention};

use crate::common;

const BASE: u64 = 0x1000;
/// The word before the GOT, holding `blrl`.
const THUNK: u64 = 0x1104;
// mflr r0; bl 0x1104; mflr r3; mtlr r0; blr
const WORDS: [u32; 5] = [
    0x7c08_02a6,
    0x4800_0101,
    0x7c68_02a6,
    0x7c08_03a6,
    0x4e80_0020,
];

/// `powerpc_sysv32` less LR in its callee-saved registers.
fn clobbers_lr(regs: &rsleigh::SleighRegs) -> BuiltCallingConvention {
    let mut cc = CallingConvention::powerpc_sysv32().build(regs).expect("cc");
    let lr = cc.link_register.expect("LR");
    cc.callee_saved_regs.retain(|&vn| vn != lr);
    cc
}

/// The thunk's convention: no arguments or results, memory and every GPR but
/// the stack pointer preserved, LR not.
fn got_thunk(regs: &rsleigh::SleighRegs) -> BuiltCallingConvention {
    let gprs = (0..32).filter(|&i| i != 1).map(|i| format!("r{i}"));
    let cc = BuiltCallingConvention {
        arg_passing_regs: Vec::new(),
        arg_passing_regs_float: Vec::new(),
        callee_saved_regs: strider_target::calling_convention::regs_to_vns(regs, gprs)
            .expect("gprs"),
        ret_val_regs: Vec::new(),
        ret_val_regs_float: Vec::new(),
        stack_args: None,
        preserves_memory: true,
        ..clobbers_lr(regs)
    };
    cc.validate()
        .expect("a per-address convention may clobber LR");
    cc
}

fn analyze(
    per_address_ccs: impl FnOnce(&rsleigh::SleighRegs) -> Vec<(u64, BuiltCallingConvention)>,
    default: impl FnOnce(&rsleigh::SleighRegs) -> BuiltCallingConvention,
) -> anyhow::Result<strider_orchestrator::AnalyzeResult> {
    let bytes = common::words_image(&WORDS, strider_target::Endianness::Big);
    let (mut strider, _) = common::strider_over_bytes(common::Arch::Ppc32be, bytes, BASE, None);
    let regs = strider.sleigh_regs();
    let per_address_ccs = per_address_ccs(regs).into_iter().collect();
    let cc = default(regs);
    let opts = strider_orchestrator::AnalyzeOptions {
        per_address_ccs: &per_address_ccs,
        ..Default::default()
    };
    strider.analyze(BASE, &cc, &opts, None)
}

#[test]
fn a_call_whose_convention_clobbers_lr_leaves_mflr_unknown() {
    let preset =
        |regs: &rsleigh::SleighRegs| CallingConvention::powerpc_sysv32().build(regs).unwrap();
    let result = analyze(|regs| vec![(THUNK, got_thunk(regs))], preset).expect("analyze");
    assert!(result.incomplete().is_empty(), "{:?}", result.incomplete());
    let f = &result.function;
    let r3 = common::returned(f)[0];
    assert_eq!(
        f.int_const_u128(r3),
        None,
        "r3 must not fold to the bl's return address"
    );
    assert!(
        matches!(
            common::producer_kind(f, r3),
            strider_ir::node::NodeKind::Call { .. }
        ),
        "r3 is the call's LR: {:?}",
        common::producer_kind(f, r3)
    );
}

/// Under the preset, LR survives the call: the same function returns the
/// `bl`'s return address, which is what the override corrects.
#[test]
fn the_preset_keeps_lr_across_the_call() {
    let preset =
        |regs: &rsleigh::SleighRegs| CallingConvention::powerpc_sysv32().build(regs).unwrap();
    let result = analyze(|_| Vec::new(), preset).expect("analyze");
    let f = &result.function;
    assert_eq!(f.int_const_u128(common::returned(f)[0]), Some(0x1008));
}

/// The function's own convention keeps LR callee-saved, or a return through
/// it is lost at the first call.
#[test]
fn a_default_convention_clobbering_lr_is_refused() {
    let Err(err) = analyze(|_| Vec::new(), clobbers_lr) else {
        panic!("a default convention without LR callee-saved lifted");
    };
    assert!(format!("{err:#}").contains("callee_saved_regs"), "{err:#}");
}
