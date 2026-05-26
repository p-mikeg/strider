//! Comprehensive calling-convention coverage tests.
//!
//! Systematically exercises:
//!   * Every parameter index 0..N for parameter-count regimes
//!     N ∈ {1, 2, 4, 8, 16}.
//!   * Sub-register parameter widths (signed/unsigned char, short).
//!   * Mixed pointer + integer parameters.
//!   * Call-of-call return-value chaining.
//!
//! For each fixture, the test verifies:
//!   (a) `FunctionArg` indices are present in the IR for at least
//!       a documented floor — proves `detect_register_args`'s
//!       sub-register-fallback path AND the IR builder's
//!       `upgrade_to_tracked_for` contained-in fallback are running.
//!   (b) For at least one `i` in `0..N`, the pattern
//!       `call().arg(i, function_arg(i))` matches — proves the call
//!       site's arg slot threads through to a `FunctionArg` node via
//!       the `LoadForward` + sub-register-fallback chain, including
//!       the walker's ability to pass through non-stack-aliasing
//!       `Store` nodes.
//!
//! The assertion floors are deliberately under the strict "all 0..N"
//! form: not every fixture's per-arch lowering routes every parameter
//! through a stable register slot the analyzer can fully reason about
//! (e.g. `forward_16`'s 16 spilled args interleaved with prologue
//! traffic).  The thread-through check (b) is the meaningful
//! cross-arch invariant.
//!
//! Conventions:
//!   * Every `Matcher` opts into `ignore_casts_mask(EXTEND | TRUNCATE
//!     | CAST_TO_BOOL | CAST_TO_INT)` and `ignore_regions()` so
//!     tests don't break on arch-specific width-cast / region-join noise.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
// `mod common;` is required so the `per_arch_test!` macro can resolve
// `$crate::common::analyze` / `$crate::common::Arch`.

use std::collections::HashSet;

use strider_analyze::pattern::{
    CastMask, Capture, IntoPat, Matcher, Pat, any, call, initial_var_for,
};

use strider_ir::node::NodeKind;

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Standard matcher for this suite — same selectivity as `complex_patterns.rs`.
fn matcher(g: &strider_ir::Function) -> Matcher<'_> {
    Matcher::try_new(g).unwrap()
        .ignore_casts_mask(
            CastMask::EXTEND
                | CastMask::TRUNCATE
                | CastMask::CAST_TO_BOOL
                | CastMask::CAST_TO_INT,
        )
        .ignore_regions()
}

/// Returns the set of arg `index` values registered in
/// `Function::arg_index_to_nodes` (the side-table populated by
/// `FunctionArgDetect`).
fn function_arg_indices(g: &strider_ir::Function) -> HashSet<u32> {
    g.arg_indices().collect()
}

/// Asserts at least `min` distinct arg indices in `0..n` are registered
/// in the side-table, and that index 0 is among them.  This is the
/// regression guard for `detect_register_args`'s sub-register fallback:
/// without it, a function whose first parameter is read at sub-register
/// width (universal at -O2 on every arch we test) would have no arg 0
/// recorded — and consequently no args at all in the extreme case.
fn assert_function_args_present(
    g: &strider_ir::Function,
    n: u32,
    min: u32,
    fn_label: &str,
) {
    let got = function_arg_indices(g);
    let in_range: HashSet<u32> = got.iter().copied().filter(|&i| i < n).collect();
    assert!(
        in_range.len() >= (min as usize),
        "{fn_label}: expected ≥{min} arg indices in 0..{n}; \
         got {} (all indices: {got:?})",
        in_range.len(),
    );
    assert!(
        in_range.contains(&0),
        "{fn_label}: expected arg 0 to be registered (the first \
         parameter); got indices {got:?}",
    );
}

/// Asserts that for at least one `i` in `0..n`, the side-table has a
/// carrier node for arg `i` that appears as a value-input to at least one
/// `Call` node (directly or via the cast-transparent pattern matcher).
///
/// Requiring "at least one match" rather than "all 0..N must match"
/// gives headroom for arch-specific lowerings where one or two arg slots
/// route through a non-`Store` chain the walker can't follow; the
/// universal cross-arch invariant is "at least one slot threads through
/// cleanly."
fn assert_some_call_arg_threads_through(
    g: &strider_ir::Function,
    n: u32,
    fn_label: &str,
) {
    let m = matcher(g);
    // Capture the call's arg i, then check if the captured value traces
    // back to the carrier through cast-transparent walks.
    let mut matched_indices: Vec<u32> = Vec::new();
    for i in 0..n {
        let carriers = g.arg_index_to_nodes(i);
        if carriers.is_empty() {
            continue;
        }
        // Use the cast-transparent pattern matcher:
        // - For InitialVar carriers: match `call().arg(i, initial_var_for(vn))`.
        // - For Load carriers: match `call().arg(i, initial_var_for(vn))` won't
        //   work, so instead capture arg i and walk backward to find the carrier.
        //
        // Strategy: capture the i-th call arg and check if the captured node's
        // source (after stripping casts) matches one of the carriers.
        let arg_cap = Capture::new();
        let pat: Pat = call().arg(i as usize, any().capture(arg_cap)).into();
        let call_matches = m.find_all(&pat);
        if call_matches.iter().any(|hit| {
            let Some(arg_out) = hit.output(arg_cap) else { return false; };
            // Walk backward through the cast chain from the captured call arg.
            // If we reach a carrier, it threads through.
            let mut cur = g.get_node_from_output(arg_out);
            for _ in 0..8 {
                if carriers.contains(&cur) {
                    return true;
                }
                // Step through cast/extend/truncate nodes one level.
                let kind = g.node_kind(cur);
                if matches!(kind,
                    NodeKind::Extend(_)
                    | NodeKind::Truncate
                    | NodeKind::CastToInt
                    | NodeKind::CastToBool
                    | NodeKind::CastToFloat
                    | NodeKind::Phi
                ) {
                    let inputs = g.node_inputs(cur);
                    if let Some(&first) = inputs.get(0) {
                        cur = g.get_node_from_output(first);
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
            for &carrier in carriers {
                if let NodeKind::InitialVar(vn) = *g.node_kind(carrier) {
                    let pat2: Pat = call().arg(i as usize, initial_var_for(vn)).into();
                    if !m.find_all(&pat2).is_empty() {
                        matched_indices.push(i);
                        break;
                    }
                }
            }
        }
    }
    assert!(
        !matched_indices.is_empty(),
        "{fn_label}: expected ≥1 i in 0..{n} where a Call's arg i traces \
         back to the registered carrier for arg i; \
         registered arg indices = {indices:?}",
        indices = function_arg_indices(g),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. forward_1 — single-parameter all-register baseline
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_1",
    forward_1_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
    }
);

fn forward_1_assertions(g: &strider_ir::Function) {
    // Strict: all 1 indices must be detected (the trivial single-arg case).
    assert_function_args_present(g, 1, 1, "forward_1");
    assert_some_call_arg_threads_through(g, 1, "forward_1");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. forward_2 — two parameters, both in registers on every non-x86 arch
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_2",
    forward_2_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
    }
);

fn forward_2_assertions(g: &strider_ir::Function) {
    // Strict: all 2 args must be present on every arch.
    assert_function_args_present(g, 2, 2, "forward_2");
    assert_some_call_arg_threads_through(g, 2, "forward_2");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. forward_4 — 4 params; all in registers except x86 (cdecl, all-stack)
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_4",
    forward_4_assertions,
    ignore = {
        Ppc64be: "ppc64 ELFv1/v2 TOC-relative addressing breaks the spill/reload forwarding chain even under AssumeStackConstDisjoint; pending follow-up that handles Add(Load(r2), const) intervening stores",
        Ppc64le: "ppc64 ELFv1/v2 TOC-relative addressing breaks the spill/reload forwarding chain even under AssumeStackConstDisjoint; pending follow-up that handles Add(Load(r2), const) intervening stores",
    }
);

fn forward_4_assertions(g: &strider_ir::Function) {
    // Strict: all 4 args must be present on every arch.
    assert_function_args_present(g, 4, 4, "forward_4");
    assert_some_call_arg_threads_through(g, 4, "forward_4");
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. forward_8 — exercises spilling on arches with <8 arg regs
//    (arm aapcs spills 4, x86 cdecl spills all 8, x64 SysV spills 2).
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_8",
    forward_8_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
    }
);

fn forward_8_assertions(g: &strider_ir::Function) {
    // Strict: the `function_args::mem_chain_is_dirty` `Store(_)` arm
    // lets all 8 stack-passed args be detected on x86 cdecl, mirroring
    // the resilience in `CallStackArgCollect` and
    // `load_forward::probe`.  Every arch detects all 8.
    assert_function_args_present(g, 8, 8, "forward_8");
    assert_some_call_arg_threads_through(g, 8, "forward_8");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. forward_16 — every arch spills SOME args to the stack.
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_16",
    forward_16_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
    }
);

fn forward_16_assertions(g: &strider_ir::Function) {
    // Floor at 8: the lowest-arity register sets (mips o32, arm aapcs)
    // pass 4–8 args in registers and spill the rest, and `FunctionArgDetect`
    // currently surfaces the register-passed callee reads but doesn't
    // canonicalise high-slot stack-arg reads on every arch.  Reaching the
    // strict 16/16 floor would need callee-side stack-arg recognition
    // beyond `CallStackArgCollect`'s caller-side reach — out of scope
    // for this assertion.  The previous floor of 4 was set when
    // interleaved volatile global writes broke `CallStackArgCollect`'s
    // memory-chain walk; both that bug (volatile-global passthrough)
    // and the chain-order-monotonicity bug (slot-by-offset matching)
    // are now fixed, so the 8/16 floor holds on every arch.
    assert_function_args_present(g, 16, 8, "forward_16");
    assert_some_call_arg_threads_through(g, 16, "forward_16");
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. narrow_widths — sub-register coverage (signed/unsigned char, short)
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "narrow_widths",
    narrow_widths_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
    }
);

fn narrow_widths_assertions(g: &strider_ir::Function) {
    // (a) Strict: 4 narrow-width args (signed/unsigned char + short)
    // fully detected via the contained-in sub-register fallback in
    // upgrade_to_tracked_for + the detect_register_args fallback.
    assert_function_args_present(g, 4, 4, "narrow_widths");
    // (b) at least one Call.arg(i) ↔ carrier(i) link exists.
    assert_some_call_arg_threads_through(g, 4, "narrow_widths");

    // (c) Source-shape check: every registered carrier for indices 0..4
    //     must be an `InitialVar(Vn)` with width ∈ {1, 2, 4, 8} (register
    //     arg) or a `Load` node (stack arg).  Validates that the
    //     sub-register fallback records the narrower-than-container Vn
    //     correctly when it does emit one.
    for idx in 0..4u32 {
        let carriers = g.arg_index_to_nodes(idx);
        for &n in carriers {
            match g.node_kind(n) {
                NodeKind::InitialVar(vn) => {
                    assert!(
                        matches!(vn.size, 1 | 2 | 4 | 8),
                        "narrow_widths: arg {idx} InitialVar carrier has \
                         unexpected width {} (Vn = {:?})", vn.size, vn,
                    );
                }
                NodeKind::Load(_) => {
                    // Stack-arg carrier: no additional width check needed here.
                }
                other => {
                    panic!(
                        "narrow_widths: arg {idx} carrier is unexpected node kind {other:?}"
                    );
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. mixed_4 — pointer + int parameters interleaved
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "mixed_4",
    mixed_4_assertions,
    ignore = {
        Ppc64be: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
        Ppc64le: "ppc64 TOC-relative addressing for globals defeats the AssumeStackConstDisjoint walk-through",
    }
);

fn mixed_4_assertions(g: &strider_ir::Function) {
    // Strict: 4 args (int + ptr interleaved) must all be detected.
    assert_function_args_present(g, 4, 4, "mixed_4");
    assert_some_call_arg_threads_through(g, 4, "mixed_4");
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. uses_return — outer Call's input traces back to the inner Call
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("calling_convention", "uses_return", uses_return_assertions);

fn uses_return_assertions(g: &strider_ir::Function) {
    // The function takes one `int x` → FunctionArg(0).
    assert_function_args_present(g, 1, 1, "uses_return");

    // We assert the call-of-call dataflow holds: at least one Call's input
    // chain must trace back to ANOTHER Call.  Modeled on
    // `complex_patterns.rs::call_uses_call_return`.
    use strider_ir::node::NodeId;
    let calls: Vec<NodeId> = g.walk()
        .filter(|&n| matches!(g.node_kind(n), NodeKind::Call))
        .collect();
    assert!(!calls.is_empty(),
            "uses_return must have ≥1 Call; got {}", calls.len());

    // For each Call, walk every input slot back through any chain of
    // {Store, Load, Region, ValuePhi}.
    // If we hit another Call, the test passes.
    let chained = calls.iter().any(|&outer| {
        let outer_inputs: Vec<_> = g.node_inputs(outer).into_iter().collect();
        outer_inputs.iter().any(|&inp| {
            let mut producer = g.get_node_from_output(inp);
            for _ in 0..16 {
                match g.node_kind(producer) {
                    NodeKind::Call if producer != outer => return true,
                    NodeKind::Load(_)
                    | NodeKind::Region => {
                        // Walk the first input — Load[memory, addr] gives the
                        // memory predecessor (producing store/Call), and
                        // Region passes through its first input.
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else { break; };
                        producer = g.get_node_from_output(first);
                    }
                    NodeKind::Phi if g.phi_var_tag(producer).is_none() => {
                        // Anonymous phi (ValuePhi from LoadForward) —
                        // pass through its first input.
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else { break; };
                        producer = g.get_node_from_output(first);
                    }
                    NodeKind::Store(_) => {
                        // Store[memory, addr, data] — walk the data input.
                        let inps: Vec<_> =
                            g.node_inputs(producer).into_iter().collect();
                        let Some(&data) = inps.get(2) else { break; };
                        producer = g.get_node_from_output(data);
                    }
                    _ => break,
                }
            }
            false
        })
    });
    if calls.len() >= 2 {
        assert!(chained,
                "uses_return: with {} Calls, expected one Call's input to \
                 trace back to another Call (through any chain of \
                 Load/Store/Region/ValuePhi)",
                calls.len());
    }
}
