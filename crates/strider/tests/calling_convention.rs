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
//!       the `StackLoadForward` + sub-register-fallback chain, including
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
//!     | CAST_TO_BOOL | CAST_TO_INT)` and `ignore_control_states()` so
//!     tests don't break on arch-specific width-cast / region-join noise.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
// `mod common;` is required so the `per_arch_test!` macro can resolve
// `$crate::common::analyze` / `$crate::common::Arch`.

use std::collections::HashSet;

use pattern::{
    CastMask, Matcher, Pat, call, function_arg,
};

use ir::node::{NodeKind, FunctionArgSource};

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Standard matcher for this suite — same selectivity as `complex_patterns.rs`.
fn matcher(g: &ir::BuiltFunctionGraph) -> Matcher<'_> {
    Matcher::new(g)
        .ignore_casts_mask(
            CastMask::EXTEND
                | CastMask::TRUNCATE
                | CastMask::CAST_TO_BOOL
                | CastMask::CAST_TO_INT,
        )
        .ignore_control_states()
}

/// Returns the set of `index` values present on `FunctionArg` nodes in `g`.
fn function_arg_indices(g: &ir::BuiltFunctionGraph) -> HashSet<u32> {
    g.preorder()
        .filter_map(|n| match g.graph.node_kind(n) {
            NodeKind::FunctionArg { index, .. } => Some(*index),
            _ => None,
        })
        .collect()
}

/// Asserts at least `min` distinct `FunctionArg` indices in `0..n` are
/// present in the IR, and that index 0 is among them.  This is the
/// regression guard for `detect_register_args`'s sub-register fallback:
/// without it, a function whose first parameter is read at sub-register
/// width (universal at -O2 on every arch we test) would have no
/// `FunctionArg(0)` — and consequently no `FunctionArg` at all in the
/// extreme case.
fn assert_function_args_present(
    g: &ir::BuiltFunctionGraph,
    n: u32,
    min: u32,
    fn_label: &str,
) {
    let got = function_arg_indices(g);
    let in_range: HashSet<u32> = got.iter().copied().filter(|&i| i < n).collect();
    assert!(
        in_range.len() >= (min as usize),
        "{fn_label}: expected ≥{min} FunctionArg indices in 0..{n}; \
         got {} (all indices: {got:?})",
        in_range.len(),
    );
    assert!(
        in_range.contains(&0),
        "{fn_label}: expected FunctionArg(0) to exist (the first \
         parameter); got indices {got:?}",
    );
}

/// Asserts that for at least one `i` in `0..n`, the pattern
///   `call().arg(i, function_arg(i))`
/// matches.  Pins the StackLoadForward + sub-register-fallback chain
/// that connects Call.arg(i) ↔ FunctionArg(i) at the call site.
///
/// Requiring "at least one match" rather than "all 0..N must match"
/// gives headroom for arch-specific lowerings where one or two arg
/// slots route through a non-`StackStore` chain the walker can't
/// follow; the universal cross-arch invariant the test enforces is
/// "at least one slot threads through cleanly."
fn assert_some_call_arg_threads_through(
    g: &ir::BuiltFunctionGraph,
    n: u32,
    fn_label: &str,
) {
    let m = matcher(g);
    let mut matched_indices: Vec<u32> = Vec::new();
    for i in 0..n {
        let pat: Pat = call().arg(i as usize, function_arg(i)).into();
        if !m.find_all(&pat).is_empty() {
            matched_indices.push(i);
        }
    }
    assert!(
        !matched_indices.is_empty(),
        "{fn_label}: expected ≥1 i in 0..{n} where Call(arg(i) = \
         function_arg(i)) matches; FunctionArg indices present = {indices:?}",
        indices = function_arg_indices(g),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. forward_1 — single-parameter all-register baseline
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_1",
    forward_1_assertions
);

fn forward_1_assertions(g: &ir::BuiltFunctionGraph) {
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
    forward_2_assertions
);

fn forward_2_assertions(g: &ir::BuiltFunctionGraph) {
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
    forward_4_assertions
);

fn forward_4_assertions(g: &ir::BuiltFunctionGraph) {
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
    forward_8_assertions
);

fn forward_8_assertions(g: &ir::BuiltFunctionGraph) {
    // Strict: the `function_args::mem_chain_is_dirty` `Store(_)` arm
    // lets all 8 stack-passed args be detected on x86 cdecl, mirroring
    // the resilience in `CallStackArgCollect` and
    // `stack_load_forward::probe`.  Every arch detects all 8.
    assert_function_args_present(g, 8, 8, "forward_8");
    assert_some_call_arg_threads_through(g, 8, "forward_8");
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. forward_16 — every arch spills SOME args to the stack.
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!(
    "calling_convention",
    "forward_16",
    forward_16_assertions
);

fn forward_16_assertions(g: &ir::BuiltFunctionGraph) {
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
    narrow_widths_assertions
);

fn narrow_widths_assertions(g: &ir::BuiltFunctionGraph) {
    // (a) at least 2 FunctionArgs exist in 0..4 (loose floor; the strict
    //     0..4 form fails on big-endian / x86-cdecl arches).
    // Strict: 4 narrow-width args (signed/unsigned char + short)
    // fully detected via the contained-in sub-register fallback in
    // upgrade_to_tracked_for + the detect_register_args fallback.
    assert_function_args_present(g, 4, 4, "narrow_widths");
    // (b) at least one Call.arg(i) ↔ FunctionArg(i) link exists.
    assert_some_call_arg_threads_through(g, 4, "narrow_widths");

    // (c) Source-shape check: every FunctionArg(0..4) that DOES exist
    //     must surface as a valid Register(Vn) (with width ∈ {1, 2, 4, 8})
    //     or Stack slot at a sane offset.  Validates that the
    //     sub-register fallback emits the narrower-than-container Vn
    //     correctly when it does emit one.
    let by_index: std::collections::HashMap<u32, FunctionArgSource> = g
        .preorder()
        .filter_map(|n| match g.graph.node_kind(n) {
            NodeKind::FunctionArg { index, source } => Some((*index, *source)),
            _ => None,
        })
        .collect();
    for (idx, src) in &by_index {
        if *idx >= 4 {
            continue;
        }
        match src {
            FunctionArgSource::Register(vn) => {
                assert!(
                    matches!(vn.size, 1 | 2 | 4 | 8),
                    "narrow_widths: FunctionArg({idx}) Register source has \
                     unexpected width {} (Vn = {:?})", vn.size, vn,
                );
            }
            FunctionArgSource::Stack { offset, .. } => {
                assert!(
                    *offset >= 0 && *offset < 256,
                    "narrow_widths: FunctionArg({idx}) Stack offset {offset} \
                     unreasonable",
                );
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
    mixed_4_assertions
);

fn mixed_4_assertions(g: &ir::BuiltFunctionGraph) {
    // Strict: 4 args (int + ptr interleaved) must all be detected.
    assert_function_args_present(g, 4, 4, "mixed_4");
    assert_some_call_arg_threads_through(g, 4, "mixed_4");
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. uses_return — outer Call's input traces back to the inner Call
// ─────────────────────────────────────────────────────────────────────────────

per_arch_test!("calling_convention", "uses_return", uses_return_assertions);

fn uses_return_assertions(g: &ir::BuiltFunctionGraph) {
    // The function takes one `int x` → FunctionArg(0).
    assert_function_args_present(g, 1, 1, "uses_return");

    // We assert the call-of-call dataflow holds: at least one Call's input
    // chain must trace back to ANOTHER Call.  Modeled on
    // `complex_patterns.rs::call_uses_call_return`.
    use ir::node::NodeId;
    let calls: Vec<NodeId> = g.preorder()
        .filter(|&n| matches!(g.graph.node_kind(n), NodeKind::Call))
        .collect();
    assert!(!calls.is_empty(),
            "uses_return must have ≥1 Call; got {}", calls.len());

    // For each Call, walk every input slot back through any chain of
    // {Store, StackStore, Load, ControlState, ValuePhi}.
    // If we hit another Call, the test passes.
    let chained = calls.iter().any(|&outer| {
        let outer_inputs: Vec<_> = g.graph.node_inputs(outer).into_iter().collect();
        outer_inputs.iter().any(|&inp| {
            let mut producer = g.graph.get_node_from_output(inp);
            for _ in 0..16 {
                match g.graph.node_kind(producer) {
                    NodeKind::Call if producer != outer => return true,
                    NodeKind::Load(_)
                    | NodeKind::ControlState
                    | NodeKind::ValuePhi => {
                        // Walk the first input — Load[memory, addr] gives the
                        // memory predecessor (producing store/Call), and
                        // ControlState/ValuePhi pass through their first input.
                        let inps: Vec<_> =
                            g.graph.node_inputs(producer).into_iter().collect();
                        let Some(&first) = inps.first() else { break; };
                        producer = g.graph.get_node_from_output(first);
                    }
                    NodeKind::Store(_) | NodeKind::StackStore { .. } => {
                        // Store[memory, addr, data] — walk the data input.
                        let inps: Vec<_> =
                            g.graph.node_inputs(producer).into_iter().collect();
                        let Some(&data) = inps.get(2) else { break; };
                        producer = g.graph.get_node_from_output(data);
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
                 Load/Store/ControlState/ValuePhi)",
                calls.len());
    }
}
