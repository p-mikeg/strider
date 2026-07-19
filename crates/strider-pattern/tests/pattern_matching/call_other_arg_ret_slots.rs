//! `CallOtherPat` input-slot patterns.
//!
//! Control and memory predecessors are not value edges under the bipartite
//! model, so they need the typed `ctrl` / `mem` aliases, which relax the
//! sub-pattern's root to a control / memory edge. `arg(idx, ..)` stays
//! value-typed for the pcode-explicit argument slots.

use strider_ir::node::ValueType;
use strider_ir::{Function, FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{Capture, CaptureExt, Matcher, any, call_other, int_const, mem_phi};

/// One `cpuid` CallOther with its pcode-explicit inputs/outputs bound through
/// real Vns, so every slot is pattern-matchable.
fn build_cpuid_graph() -> Function {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    // CPUID is empty-channel with memory_edge=true per the ABI table.
    let _ = b
        .build_call_other_abi(
            7,
            "cpuid",
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
            },
            None,
            false,
        )
        .expect("cpuid");
    b.build_return(None, &[]).expect("return");
    b.build().expect("FunctionBuilder::build")
}

#[test]
fn ctrl_alias_binds_control_predecessor() {
    let function = build_cpuid_graph();
    // inputs[0] is the region's control output; ctrl() relaxes the wildcard's
    // root to a control edge so it can bind there.
    let c = Capture::new();
    let pat = call_other().name("cpuid").ctrl(any().capture(c)).build();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].node(c, function.graph()).is_some(),
        "ctrl input capture must bind to a real producer node",
    );
}

#[test]
fn mem_alias_binds_memory_predecessor() {
    let function = build_cpuid_graph();
    // inputs[1] is the region's MemPhi token.
    let c = Capture::new();
    let pat = call_other().name("cpuid").mem(mem_phi().capture(c)).build();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].node(c, function.graph()).is_some(),
        "mem input capture must bind"
    );
}

#[test]
fn arg_constrains_pcode_explicit_value_argument() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let a0 = b.build_int_const(0x11u64, ValueType::I64).expect("a0");
    // Inputs are [ctrl(0), mem(1), arg0(2)].
    let _ = b
        .build_call_other_abi(
            9,
            "rdmsr",
            &[a0],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
            },
            None,
            false,
        )
        .expect("rdmsr");
    b.build_return(None, &[]).expect("return");
    let function = b.build().expect("build");
    let matcher = Matcher::new(&function);

    assert_eq!(
        matcher
            .find_all(
                &call_other()
                    .name("rdmsr")
                    .arg(2, int_const(0x11u128))
                    .build()
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        matcher
            .find_all(
                &call_other()
                    .name("rdmsr")
                    .arg(2, int_const(0x99u128))
                    .build()
            )
            .unwrap()
            .len(),
        0
    );
}
