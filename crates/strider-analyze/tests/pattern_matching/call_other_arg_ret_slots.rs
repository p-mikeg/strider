//! `CallOtherPat::arg(i)` / `ret(i)` address raw input/output slots so
//! patterns can constrain ctrl, mem, pcode-explicit args, implicit
//! reads, value, and clobber slots uniformly.  Convenience aliases
//! (`ctrl`, `mem`, `ctrl_out`, `mem_out`) are thin shortcuts.

use strider_analyze::pattern::{Capture, IntoPat, Matcher, any, call_other};
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
        .build_call_other_modeled(7, "cpuid", &[], None, &[], &[], &[])
        .expect("cpuid");
    b.build_return(None, &[]).expect("return");
    b.build().expect("FunctionBuilder::build")
}

#[test]
fn arg_zero_matches_control_input() {
    let function = build_cpuid_graph();
    // Capture inputs[0] (ctrl) — should bind to *some* control producer.
    let c = Capture::new();
    let pat = call_other().name("cpuid").arg(0, any().capture(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].node(c).is_some(),
        "ctrl input capture must bind to a real producer node",
    );
}

#[test]
fn arg_one_matches_memory_input() {
    let function = build_cpuid_graph();
    let c = Capture::new();
    let pat = call_other().name("cpuid").arg(1, any().capture(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].node(c).is_some(), "mem input capture must bind");
}

#[test]
fn ret_zero_matches_control_output_consumer_when_present() {
    // For cpuid (memory_edge=true), the post-cpuid Return reads ctrl_out;
    // ret(0, _) matches the control output side of the CallOther.  We
    // capture and verify the bound output corresponds to the CallOther
    // node (not its predecessor).
    let function = build_cpuid_graph();
    let c = Capture::new();
    let pat = call_other().name("cpuid").ret(0, any().capture(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(hits.len(), 1);
    let captured_node = hits[0].node(c).expect("ret(0) capture binds");
    assert_eq!(
        captured_node,
        hits[0].root(),
        "ret(0) is an output of the matched CallOther itself",
    );
}

#[test]
fn ctrl_alias_equals_arg_zero() {
    let function = build_cpuid_graph();
    let c1 = Capture::new();
    let c2 = Capture::new();
    let pat_arg = call_other().name("cpuid").arg(0, any().capture(c1));
    let pat_alias = call_other().name("cpuid").ctrl(any().capture(c2));
    let arg_hits = Matcher::try_new(&function).unwrap().find_all(&pat_arg.into());
    let alias_hits = Matcher::try_new(&function).unwrap().find_all(&pat_alias.into());
    assert_eq!(arg_hits.len(), 1);
    assert_eq!(alias_hits.len(), 1);
    assert_eq!(arg_hits[0].node(c1), alias_hits[0].node(c2));
}

#[test]
fn mem_alias_equals_arg_one() {
    let function = build_cpuid_graph();
    let c1 = Capture::new();
    let c2 = Capture::new();
    let pat_arg = call_other().name("cpuid").arg(1, any().capture(c1));
    let pat_alias = call_other().name("cpuid").mem(any().capture(c2));
    let arg_hits = Matcher::try_new(&function).unwrap().find_all(&pat_arg.into());
    let alias_hits = Matcher::try_new(&function).unwrap().find_all(&pat_alias.into());
    assert_eq!(arg_hits[0].node(c1), alias_hits[0].node(c2));
}

#[test]
fn arg_and_ret_compose_in_one_pattern() {
    // Constrain control predecessor AND control output simultaneously.
    let function = build_cpuid_graph();
    let c_in = Capture::new();
    let c_out = Capture::new();
    let pat = call_other()
        .name("cpuid")
        .arg(0, any().capture(c_in))
        .ret(0, any().capture(c_out));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].node(c_in).is_some());
    assert!(hits[0].node(c_out).is_some());
    // ctrl_in and ctrl_out are different nodes (input is the predecessor,
    // output is the CallOther itself).
    assert_ne!(hits[0].node(c_in), hits[0].node(c_out));
}
