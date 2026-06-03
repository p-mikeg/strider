//! `CallOtherPat` input-slot patterns: the `ctrl` / `mem` aliases bind
//! the control / memory predecessors, and `arg(idx, value_pat)`
//! constrains a pcode-explicit value argument.
//!
//! Under the bipartite model the control and memory predecessors are
//! NOT value edges, so they are addressed through the typed `ctrl` /
//! `mem` aliases (which relax the sub-pattern's root to a control /
//! memory edge); `arg(idx, …)` stays value-typed for the pcode-explicit
//! argument slots.

use strider_pattern::{Capture, CaptureExt, Matcher, any, call_other, int_const, mem_phi};
use strider_ir::node::ValueType;
use strider_ir::{Function, FunctionBuilder};
use strider_ir_test_utils::RegisterSet;

/// Build a graph with a single `cpuid` CallOther whose pcode-explicit
/// inputs/outputs are bound through real Vns so we can pattern-match
/// each slot.
fn build_cpuid_graph() -> Function {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    // CPUID per the ABI table is empty-channel + memory_edge=true.
    // Pass no pcode-explicit args, no implicit reads, no implicit writes.
    let _ = b
        .build_call_other(7, "cpuid", None, &[], &strider_target::BuiltCallOtherAbi { implicit_reads: Vec::new(), implicit_writes: Vec::new(), clobbers_memory: false }, None, false)
        .expect("cpuid");
    b.build_return(None, &[]).expect("return");
    b.build().expect("FunctionBuilder::build")
}

#[test]
fn ctrl_alias_binds_control_predecessor() {
    let function = build_cpuid_graph();
    // The control predecessor (inputs[0]) is the region's control
    // output; `ctrl(any())` relaxes the sub-pattern's root to a control
    // edge so the wildcard binds it.
    let c = Capture::new();
    let pat = call_other().name("cpuid").ctrl(any().capture(c)).build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].node(c, function.graph()).is_some(),
        "ctrl input capture must bind to a real producer node",
    );
}

#[test]
fn mem_alias_binds_memory_predecessor() {
    let function = build_cpuid_graph();
    // The memory predecessor (inputs[1]) is the region's MemPhi token.
    let c = Capture::new();
    let pat = call_other().name("cpuid").mem(mem_phi().capture(c)).build();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].node(c, function.graph()).is_some(), "mem input capture must bind");
}

/// `arg(idx, value_pat)` constrains a pcode-explicit value argument.
#[test]
fn arg_constrains_pcode_explicit_value_argument() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let a0 = b.build_int_const(0x11u64, ValueType::I64).expect("a0");
    // A modeled CallOther with one pcode-explicit value arg.  Its inputs
    // are `[ctrl(0), mem(1), arg0(2)]`.
    let _ = b
        .build_call_other(9, "rdmsr", None, &[a0], &strider_target::BuiltCallOtherAbi { implicit_reads: Vec::new(), implicit_writes: Vec::new(), clobbers_memory: false }, None, false)
        .expect("rdmsr");
    b.build_return(None, &[]).expect("return");
    let function = b.build().expect("build");
    let matcher = Matcher::try_new(&function).unwrap();

    // arg slot 2 holds the value argument IntConst(0x11).
    assert_eq!(
        matcher.find_all(&call_other().name("rdmsr").arg(2, int_const(0x11u128)).build()).len(),
        1
    );
    // Wrong value at the same slot → reject.
    assert_eq!(
        matcher.find_all(&call_other().name("rdmsr").arg(2, int_const(0x99u128)).build()).len(),
        0
    );
}
