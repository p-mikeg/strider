//! Calling-convention coverage across parameter counts N in {1, 2, 4, 8, 16},
//! sub-register widths (signed/unsigned char, short), mixed pointer+int
//! params, and call-of-call return chaining.
//!
//! Each fixture checks (a) arg indices are registered in the IR for at
//! least a floor count (regression guard for the builder-entry
//! sub-register fallback in `set_entry_region`'s `read_reg_vn`), and
//! (b) at least one call-site arg slot threads back to its carrier
//! through the `LoadForward` + sub-register-fallback chain.
//!
//! The floors sit below the strict "all 0..N" mark because not every per-arch
//! lowering routes every parameter through a slot the analyzer can reason
//! about (e.g. `forward_16`'s spilled args interleaved with prologue
//! traffic). The thread-through check (b) is the invariant that holds on
//! every arch.

mod common;
// `per_arch_test!` resolves `$crate::common::analyze` / `$crate::common::Arch`.

use std::collections::HashSet;
use strider_ir::{IRViewer, IRWalker};

use strider_pattern::{
    Capture, CaptureExt, CastMask, Matcher, Pattern, anything, call, initial_var_for,
};

use strider_ir::node::NodeKind;

/// Cast selectivity mirrors `complex_patterns.rs`.
fn cast_mask() -> CastMask {
    CastMask::EXTEND | CastMask::TRUNCATE
}

fn masked(p: Pattern) -> Pattern {
    p.ignore_casts_mask(cast_mask())
}

fn matcher(function: &strider_ir::Function) -> Matcher<'_> {
    Matcher::new(function)
}

fn function_arg_indices(function: &strider_ir::Function) -> HashSet<u32> {
    function.side_tables().iter_arg_indices().collect()
}

/// Pins the builder-entry sub-register fallback: a first parameter read at
/// sub-register width, which is universal at -O2 on every arch tested, still
/// records arg 0.
fn assert_function_args_present(function: &strider_ir::Function, n: u32, min: u32, fn_label: &str) {
    let got = function_arg_indices(function);
    let in_range: HashSet<u32> = got.iter().copied().filter(|&i| i < n).collect();
    assert!(
        in_range.len() >= (min as usize),
        "{fn_label}: expected >={min} arg indices in 0..{n}; \
         got {} (all indices: {got:?})",
        in_range.len(),
    );
    assert!(
        in_range.contains(&0),
        "{fn_label}: expected arg 0 to be registered (the first \
         parameter); got indices {got:?}",
    );
}

/// "At least one match" rather than "all 0..N" gives headroom for
/// arch-specific lowerings where one or two arg slots route through a
/// non-`Store` chain the walker can't follow; the universal cross-arch
/// invariant is that at least one slot threads through cleanly.
fn assert_some_call_arg_threads_through(function: &strider_ir::Function, n: u32, fn_label: &str) {
    let m = matcher(function);
    let mut matched_indices: Vec<u32> = Vec::new();
    for i in 0..n {
        let carriers = function.side_tables().arg_index_to_values(i);
        if carriers.is_empty() {
            continue;
        }
        // Capture arg i, then walk back through cast/extend/truncate/phi to
        // see if it lands on a registered carrier.
        let arg_cap = Capture::new();
        let pat = masked(call().arg(i as usize, anything().capture(arg_cap)).build());
        let call_matches = m.find_all(&pat).unwrap();
        if call_matches.iter().any(|hit| {
            let Some(arg_value) = hit.value(arg_cap) else {
                return false;
            };
            let mut cur = function.producer(arg_value);
            for _ in 0..8 {
                if carriers.iter().any(|&v| function.producer(v) == cur) {
                    return true;
                }
                let kind = function.node_kind(cur);
                if matches!(
                    kind,
                    NodeKind::Extend(_)
                        | NodeKind::Truncate
                        | NodeKind::IntBitsToFloat
                        | NodeKind::FloatBitsToInt
                        | NodeKind::Phi
                ) {
                    let inputs = function.node_inputs(cur);
                    if let Some(&first) = inputs.get(0) {
                        cur = function.producer(first);
                        continue;
                    }
                }
                break;
            }
            false
        }) {
            matched_indices.push(i);
        }

        // Also try the cast-transparent matcher for InitialVar carriers.
        if matched_indices.last() != Some(&i) {
            for &carrier_value in carriers {
                let carrier = function.producer(carrier_value);
                if let NodeKind::InitialVar(vn_id) = *function.node_kind(carrier) {
                    let vn = function.initial_vn(vn_id);
                    let pat2 = masked(call().arg(i as usize, initial_var_for(vn)).build());
                    if !m.find_all(&pat2).unwrap().is_empty() {
                        matched_indices.push(i);
                        break;
                    }
                }
            }
        }
    }
    assert!(
        !matched_indices.is_empty(),
        "{fn_label}: expected >=1 i in 0..{n} where a Call's arg i traces \
         back to the registered carrier for arg i; \
         registered arg indices = {indices:?}",
        indices = function_arg_indices(function),
    );
}

// forward_1: single-parameter, all-register baseline.
per_arch_test!(
    "calling_convention",
    "forward_1",
    forward_1_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
    }
);

fn forward_1_assertions(function: &strider_ir::Function) {
    assert_function_args_present(function, 1, 1, "forward_1");
    assert_some_call_arg_threads_through(function, 1, "forward_1");
}

// forward_2: two params, register-passed on every arch but x86 (cdecl spills).
per_arch_test!(
    "calling_convention",
    "forward_2",
    forward_2_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
    }
);

fn forward_2_assertions(function: &strider_ir::Function) {
    assert_function_args_present(function, 2, 2, "forward_2");
    assert_some_call_arg_threads_through(function, 2, "forward_2");
}

// forward_4: 4 params, all register-passed except x86 (cdecl, all-stack).
per_arch_test!(
    "calling_convention",
    "forward_4",
    forward_4_assertions,
    ignore = {
        Ppc64be: "ppc64 ELFv1/v2 TOC-relative addressing breaks the spill/reload forwarding chain even under StackGlobalDisjoint; pending follow-up that handles Add(Load(r2), const) intervening stores",
        Ppc64le: "ppc64 ELFv1/v2 TOC-relative addressing breaks the spill/reload forwarding chain even under StackGlobalDisjoint; pending follow-up that handles Add(Load(r2), const) intervening stores",
    }
);

fn forward_4_assertions(function: &strider_ir::Function) {
    assert_function_args_present(function, 4, 4, "forward_4");
    assert_some_call_arg_threads_through(function, 4, "forward_4");
}

// forward_8: exercises spilling on arches with <8 arg regs (arm aapcs spills
// 4, x86 cdecl spills all 8, x64 SysV spills 2).
per_arch_test!(
    "calling_convention",
    "forward_8",
    forward_8_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
    }
);

fn forward_8_assertions(function: &strider_ir::Function) {
    // `function_args::mem_chain_is_dirty`'s `Store(_)` arm lets all 8
    // stack-passed args be detected on x86 cdecl; every arch detects all 8.
    assert_function_args_present(function, 8, 8, "forward_8");
    assert_some_call_arg_threads_through(function, 8, "forward_8");
}

// forward_16: every arch spills some args to the stack.
per_arch_test!(
    "calling_convention",
    "forward_16",
    forward_16_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
    }
);

fn forward_16_assertions(function: &strider_ir::Function) {
    // Floor at 8, not 16: the lowest-arity register sets (mips o32, arm
    // aapcs) pass 4-8 args in registers and spill the rest, and
    // `FunctionArgDetect` does not canonicalise every high-slot stack-arg read
    // on every arch. Reaching 16/16 needs callee-side stack-arg recognition
    // beyond `CallStackArgCollect`'s caller-side reach.
    assert_function_args_present(function, 16, 8, "forward_16");
    assert_some_call_arg_threads_through(function, 16, "forward_16");
}

// narrow_widths: sub-register coverage (signed/unsigned char, short).
per_arch_test!(
    "calling_convention",
    "narrow_widths",
    narrow_widths_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
    }
);

fn narrow_widths_assertions(function: &strider_ir::Function) {
    assert_function_args_present(function, 4, 4, "narrow_widths");
    assert_some_call_arg_threads_through(function, 4, "narrow_widths");

    // Every registered carrier must be an InitialVar(Vn) of width 1/2/4/8
    // (register arg) or a Load (stack arg): checks the sub-register
    // fallback records the narrower-than-container Vn correctly.
    for idx in 0..4u32 {
        let carriers = function.side_tables().arg_index_to_values(idx);
        for &v in carriers {
            let n = function.producer(v);
            match function.node_kind(n) {
                NodeKind::InitialVar(vn_id) => {
                    let vn = function.initial_vn(*vn_id);
                    assert!(
                        matches!(vn.size, 1 | 2 | 4 | 8),
                        "narrow_widths: arg {idx} InitialVar carrier has \
                         unexpected width {} (Vn = {:?})",
                        vn.size,
                        vn,
                    );
                }
                NodeKind::Load(_) => {}
                other => {
                    panic!("narrow_widths: arg {idx} carrier is unexpected node kind {other:?}");
                }
            }
        }
    }
}

// mixed_4: pointer + int parameters interleaved.
per_arch_test!(
    "calling_convention",
    "mixed_4",
    mixed_4_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the StackGlobalDisjoint walk-through",
    }
);

fn mixed_4_assertions(function: &strider_ir::Function) {
    assert_function_args_present(function, 4, 4, "mixed_4");
    assert_some_call_arg_threads_through(function, 4, "mixed_4");
}

// uses_return: outer Call's input traces back to an inner Call's return value.
per_arch_test!("calling_convention", "uses_return", uses_return_assertions);

fn uses_return_assertions(function: &strider_ir::Function) {
    // One `int x` parameter -> arg 0.
    assert_function_args_present(function, 1, 1, "uses_return");

    // Modeled on `complex_patterns.rs::call_uses_call_return`.
    use strider_ir::node::NodeId;
    let calls: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Call))
        .collect();
    assert!(
        !calls.is_empty(),
        "uses_return must have >=1 Call; got {}",
        calls.len()
    );

    // For each Call, walk every input slot back through any chain of
    // {Store, Load, Region, ValuePhi}; hitting another Call is the proof.
    let chained = calls.iter().any(|&outer| {
        let outer_inputs: Vec<_> = function.node_inputs(outer).into_iter().collect();
        outer_inputs.iter().any(|&inp| {
            let mut producer = function.producer(inp);
            for _ in 0..16 {
                match function.node_kind(producer) {
                    NodeKind::Call if producer != outer => return true,
                    NodeKind::Load(_) | NodeKind::Region => {
                        // Load[memory, addr]: first input is the memory
                        // predecessor. Region: first input is its first pred.
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else {
                            break;
                        };
                        producer = function.producer(first);
                    }
                    NodeKind::Phi
                        if function
                            .get_vn_for_value(function.node_outputs(producer)[0])
                            .is_none() =>
                    {
                        // Anonymous phi (from LoadForward): pass through its
                        // first input.
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else {
                            break;
                        };
                        producer = function.producer(first);
                    }
                    NodeKind::Store(_) => {
                        // Store[memory, addr, data]: walk the data input.
                        let inps: Vec<_> = function.node_inputs(producer).into_iter().collect();
                        let Some(&data) = inps.get(2) else {
                            break;
                        };
                        producer = function.producer(data);
                    }
                    _ => break,
                }
            }
            false
        })
    });
    if calls.len() >= 2 {
        assert!(
            chained,
            "uses_return: with {} Calls, expected one Call's input to \
                 trace back to another Call (through any chain of \
                 Load/Store/Region/ValuePhi)",
            calls.len()
        );
    }
}
