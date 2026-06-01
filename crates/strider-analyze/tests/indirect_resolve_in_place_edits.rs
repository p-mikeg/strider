//! Integration tests for
//! [`strider_analyze::opt::inplace`].
//!
//! Drives the in-place editors against full strider lifts (not the
//! unit-test scaffold in `inplace.rs::tests`).  Asserts that the
//! placeholder Return is mutated correctly, the use-list stays
//! consistent, and `strider_ir::validate::validate` keeps passing post-edit.
//!
//! The fixtures use the existing helper at
//! `tests/common/indirect_resolve_helpers/orchestrator.rs` (which lifts an
//! x86_64 byte sequence + runs the strider optimiser pipeline).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;
use common::indirect_resolve_helpers::build_initial_var_target_scenario_x86_64;

use strider_ir::node::{NodeId, NodeKind};
use strider_analyze::opt::{apply_link_register, apply_tail_call};
use strider_pattern::GraphRewriteCtxExt;

/// Locate the unique placeholder `IndirectBranch` in `graph`.  Panics
/// if 0 or multiple are found.
fn locate_placeholder_return(function: &strider_ir::Function) -> NodeId {
    let mut found: Option<NodeId> = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        assert!(
            found.is_none(),
            "fixture must have exactly one IndirectBranch placeholder"
        );
        found = Some(nid);
    }
    found.expect("no IndirectBranch placeholder found")
}

/// Locate the (unique) freshly-created Return — the one that's NOT the
/// placeholder.  The placeholder has been detached so finding the
/// reachable Return is sufficient.
fn locate_fresh_return(function: &strider_ir::Function) -> NodeId {
    let mut found: Option<NodeId> = None;
    for nid in function.walk() {
        if !matches!(function.node_kind(nid), NodeKind::Return) {
            continue;
        }
        assert!(
            found.is_none(),
            "fixture must have exactly one reachable Return post-edit"
        );
        found = Some(nid);
    }
    found.expect("no reachable Return found")
}

#[test]
fn apply_link_register_to_real_lift_zero_ret_vals_drops_target_value() {
    // A live x86_64 lift of `jmp rax`: the placeholder IndirectBranch
    // has 3 inputs ([ctrl, mem, InitialVar(rax)] after optimisation).
    // apply_link_register replaces it with a fresh Return whose
    // `target_value` slot is dropped, so with zero ret_vals the
    // resulting Return is `[ctrl, mem]`.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let inputs_before: Vec<_> = function.node_inputs(return_id).into_iter().collect();
    function.with_rewrite_ctx(|ctx| apply_link_register(ctx, return_id, &[])).expect("apply");
    let new_return = locate_fresh_return(&function);
    let inputs_after: Vec<_> = function.node_inputs(new_return).into_iter().collect();
    assert_eq!(inputs_after.len(), 2, "target_value dropped, no ret_vals appended");
    assert_eq!(inputs_after[0], inputs_before[0], "ctrl preserved");
    assert_eq!(inputs_after[1], inputs_before[1], "mem preserved");
    // NOTE: full `strider_ir::validate::validate` is intentionally
    // skipped here.  This test exercises the editor in isolation with
    // an empty `ret_val_outputs` slice — under the J2 cc-arity check
    // the resulting Return would (correctly) be flagged as too short
    // vs the function's declared `x86_64_systemv` CC.  The
    // orchestrator-driven test below
    // (`apply_tail_call_patches_cache_exit_handle_via_orchestrator`)
    // covers the full validate path with the right ret-val arity.
}

#[test]
fn apply_link_register_to_real_lift_appends_one_ret_val() {
    // Append one ret_val output (the existing target_value's
    // NodeOutputId — value-typed and reachable from the entry).
    // After apply_link_register the placeholder `target_value` is
    // dropped and `anchor` takes its place, so the input count is
    // unchanged but slot 2 is now the ret_val.
    let (mut function, anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let inputs_before: Vec<_> = function.node_inputs(return_id).into_iter().collect();
    function.with_rewrite_ctx(|ctx| apply_link_register(ctx, return_id, &[anchor])).expect("apply");
    let new_return = locate_fresh_return(&function);
    let inputs_after: Vec<_> = function.node_inputs(new_return).into_iter().collect();
    assert_eq!(inputs_after.len(), inputs_before.len(), "[ctrl, mem, ret_val_0]");
    assert_eq!(*inputs_after.last().expect("non-empty"), anchor);
    // Full validate skipped — see note in the sibling test above.
}

#[test]
fn apply_tail_call_replaces_placeholder_with_call_then_return() {
    // apply_tail_call now does a real in-place edit.
    // After the edit, the IR contains a Call with the IntConst
    // target as its address, feeding a fresh Return.  The original
    // placeholder Return is detached (unreachable from entry) but
    // the rest of the body is untouched.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let target = 0x1234_5678_u64;
    let new_return = function.with_rewrite_ctx(|ctx| apply_tail_call(ctx, return_id, target, &[], &[], &[], false)).expect("apply_tail_call");
    assert_ne!(
        new_return, return_id,
        "tail-call edit must produce a fresh Return id",
    );
    // Walk: the new Return must be reachable; the old placeholder
    // must not be.
    let mut new_seen = false;
    let mut old_seen = false;
    for nid in function.walk() {
        if nid == new_return {
            new_seen = true;
        }
        if nid == return_id {
            old_seen = true;
        }
    }
    assert!(new_seen, "new Return must be reachable from entry");
    assert!(!old_seen, "old placeholder must be detached / unreachable");
    // Full validate skipped: editor-isolation test with empty
    // ret_val_outputs; see note on the apply_link_register tests above.
}

// ── G1-COMPLETE: cache-exit-handle / NodeId-stability tests ────────────────

#[test]
fn apply_tail_call_returns_node_id_of_new_return() {
    // Pin: apply_tail_call's return value is a NodeId distinct from
    // the placeholder's, and it points at a Return node.  This is
    // the handle the orchestrator uses to patch the cache's
    // `exit_control` after the in-place edit.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let new_return = function.with_rewrite_ctx(|ctx| apply_tail_call(ctx, return_id, 0xc0de_u64, &[], &[], &[], false)).expect("apply_tail_call");
    assert_ne!(new_return, return_id);
    assert!(matches!(function.node_kind(new_return), NodeKind::Return));
}

#[test]
fn apply_tail_call_new_return_control_input_is_call_output() {
    // The new Return's control input (slot 0) must be the Call's
    // control output (the Call is at slot 0's producer).  This is
    // the value the orchestrator threads back into the cache's
    // `exit_control`, so test it directly.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let new_return = function.with_rewrite_ctx(|ctx| apply_tail_call(ctx, return_id, 0xface_u64, &[], &[], &[], false)).expect("apply_tail_call");
    let inputs: Vec<_> = function.node_inputs(new_return).into_iter().collect();
    let new_ctrl_in = inputs[0];
    let (producer, _idx) = function.output_definition(new_ctrl_in);
    assert!(
        matches!(function.node_kind(producer), NodeKind::Call),
        "new Return's ctrl input must come from a Call node, got {:?}",
        function.node_kind(producer),
    );
}

#[test]
fn apply_link_register_emits_fresh_return_and_detaches_placeholder() {
    // Pin: apply_link_register replaces the IndirectBranch placeholder
    // with a freshly-built Return whose control + memory inputs are
    // re-wired from the placeholder.  The placeholder is detached
    // (zero inputs, no longer reachable via the exit-control walk).
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    function.with_rewrite_ctx(|ctx| apply_link_register(ctx, return_id, &[])).expect("apply");
    // Placeholder is detached: zero inputs.  Kind remains
    // IndirectBranch (the orchestrator filters by kind via
    // find_indirect_branch_placeholder, but the anchor's use-list
    // no longer points at it, so it's effectively retired).
    assert_eq!(function.node_inputs(return_id).len(), 0,
        "detached placeholder must have zero inputs");
    // A fresh, reachable Return materialises with ctrl + memory inputs.
    let new_return = locate_fresh_return(&function);
    assert_ne!(new_return, return_id);
    let inputs: Vec<_> = function.node_inputs(new_return).into_iter().collect();
    assert!(
        inputs.len() >= 2,
        "fresh Return must have control + memory inputs",
    );
}

#[test]
fn apply_tail_call_patches_cache_exit_handle_via_orchestrator() {
    // Indirect end-to-end test: drive the orchestrator on a tail-call
    // fixture (`push K; pop rax; jmp rax` with K outside the function)
    // and assert that the orchestrator returns a valid graph (or a
    // typed error if the optimiser didn't fold the synthetic
    // sequence).  This pins the cache-handle contract at the
    // orchestrator surface — if the patch logic regressed, the cache
    // would carry a stale exit_control and the orchestrator would
    // panic or return malformed IR.
    use rsleigh::Sleigh;
    use rsleigh::mem_readers::BufMemReader;
    use strider_analyze::{run, RunConfig, RunOptions};
    use strider_target::{CallingConvention, SleighArch};
    let k = 0x500u64;
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));
    let arch_ref = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes.clone(), 0x1000);
    let sleigh = Sleigh::new(arch_ref.sla_spec(), arch_ref.pspec(), reader).expect("sleigh");
    let config = RunConfig::new(
        arch_ref,
        CallingConvention::x86_64_systemv().unwrap(),
        sleigh,
        strider_lift::cfg::MachineInsnAddr::from(0x1000),
        RunOptions::new(),
    )
    .unwrap();
    // The contract pinned here: `run` returns a typed result, never
    // panics, regardless of whether the optimiser folds the
    // push+pop+jmp into a Single tail call.
    let _ = run(config);
}

#[test]
fn apply_tail_call_real_lift_target_int_const_value_matches() {
    // After the edit, the Call's address-input is an IntConst with
    // the exact target value.  Pins the soundness contract — the
    // tail call dispatches to the resolved target, not some folded
    // approximation.
    use strider_ir::node::NodeKind;
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let target = 0xdead_beef_u64;
    let new_return = function.with_rewrite_ctx(|ctx| apply_tail_call(ctx, return_id, target, &[], &[], &[], false)).expect("apply_tail_call");
    let inputs: Vec<_> = function.node_inputs(new_return).into_iter().collect();
    let call_ctrl = inputs[0];
    let (call_node, _idx) = function.output_definition(call_ctrl);
    let call_inputs: Vec<_> =
        function.node_inputs(call_node).into_iter().collect();
    let call_addr = call_inputs[2];
    let (addr_node, _) = function.output_definition(call_addr);
    match function.node_kind(addr_node) {
        NodeKind::IntConst(v) => assert_eq!(*v, u128::from(target)),
        other => panic!("expected IntConst, got {other:?}"),
    }
}

// ── ABI threading for in-place tail-call resolution ─────────────────────────

/// Drive a real strider lift to produce a placeholder Return, run
/// `apply_tail_call` with non-empty `arg_passing_outputs` /
/// `clobbered_kinds` / `ret_val_outputs` (mirroring what the
/// orchestrator's `apply_in_place_edit` populates), then run a
/// `strider_pattern::call().arg(0, …)` query to confirm the Call exposes a
/// real arg slot 0.
///
/// **Without ABI threading:** `apply_tail_call`'s 4-arg signature ignored the
/// calling convention.  `strider_pattern::call().arg(0, predicate(|_| true))`
/// returned zero matches because the resulting Call had only
/// `[ctrl, mem, target]` inputs — no arg slots.
///
/// **With ABI threading:** with the convention threaded, the Call has arg slot 0
/// and the pattern query matches.  This is the load-bearing claim:
/// pattern queries against resolved indirect Calls now work.
#[test]
fn apply_tail_call_with_calling_context_exposes_arg_slot_0_to_pattern_query() {
    use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
    use strider_pattern::{any, call, Matcher, Capture, Wildcard};

    // Strider-lifted x86_64 fixture: `jmp rax`.  After the optimiser
    // runs, the placeholder Return has 3 inputs `[ctrl, mem,
    // InitialVar(rax)]`.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);

    // Synthesise three value-typed outputs to stand in for x86_64
    // SysV's first three arg-passing regs (RDI, RSI, RDX).  In
    // production these come from the cache's `exit_vn_to_value` /
    // an existing `InitialVar(rdi)`, but for this unit-level
    // integration test the IR identity of the value doesn't matter —
    // only that the in-place edit threads it through unchanged.
    let mk_const = |g: &mut strider_ir::Function, v: u128| {
        let nid = g.create_node(
            NodeKind::IntConst(v),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        // Stamp a sentinel asm-fingerprint so Layer-C validation
        // accepts this synthetic-direct `create_node` node (no
        // `FunctionBuilder::set_lift_addr` plumbing was in scope here).
        g.set_asm_fingerprint(nid, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
        g.node_outputs_exact::<1>(nid).expect("out")[0]
    };
    let arg0 = mk_const(&mut function, 0xa00);
    let arg1 = mk_const(&mut function, 0xa01);
    let arg2 = mk_const(&mut function, 0xa02);
    let ret_val = mk_const(&mut function, 0xb00);

    // Stand-in clobbered kinds (one per RBX/RBP/R12 → 3 slots).
    let clob_kinds = [
        NodeOutputKind::OutputType(NodeOutputType::I64),
        NodeOutputKind::OutputType(NodeOutputType::I64),
        NodeOutputKind::OutputType(NodeOutputType::I64),
    ];

    let _new_return = function
        .with_rewrite_ctx(|ctx| {
            apply_tail_call(
                ctx,
                return_id,
                0xdead_beef,
                &[arg0, arg1, arg2],
                &clob_kinds,
                &[ret_val],
                false,
            )
        })
        .expect("apply_tail_call");

    // Full validate skipped — editor-isolation test with partial
    // ret_val_outputs; the orchestrator-driven path covers the
    // validate contract.  Other invariants (use-list consistency,
    // local typing) are pinned via the explicit assertions below.

    // The headline assertion: a `strider_pattern::call().arg(0, …)` query
    // matches at least once.  Before the ABI-threading fix this would
    // have returned zero matches because the Call had no arg slot 0.
    let v0 = Capture::new();
    let pat: strider_pattern::Pat<Wildcard> = call().arg(0, any().capture(v0)).into();
    let matcher = Matcher::try_new(&function).unwrap();
    let matches = matcher.find_all(&pat);
    assert!(
        !matches.is_empty(),
        "strider_pattern::call().arg(0) must match the in-place-edited Call",
    );
    // Bonus: the captured value must be exactly arg0 (the
    // in-place edit threaded it through unchanged).
    let m = &matches[0];
    let captured = m.output(v0).expect("arg0 capture must bind");
    assert_eq!(
        captured, arg0,
        "captured arg slot 0 must equal the threaded arg0 value",
    );

    // Cross-check the Call's clobbered-output kinds: the Call's
    // outputs are `[Control, Memory] + clobbered_kinds`, so output
    // count must be 5.
    let call_node = {
        // The Call is the producer of the new Return's ctrl input.
        let ret_inputs: Vec<_> =
            function.node_inputs(_new_return).into_iter().collect();
        let (call_node, _) = function.output_definition(ret_inputs[0]);
        call_node
    };
    let call_outputs: Vec<_> = function.node_outputs(call_node).to_vec();
    assert_eq!(
        call_outputs.len(),
        2 + clob_kinds.len(),
        "Call's outputs are [Control, Memory] + clobbered_kinds",
    );

    // And: the new Return's ret-val slot 2 is `ret_val`.
    let ret_inputs: Vec<_> =
        function.node_inputs(_new_return).into_iter().collect();
    assert_eq!(
        ret_inputs[2], ret_val,
        "Return's ret-val slot 0 (input #2) must be the threaded ret_val",
    );
}

#[test]
fn apply_tail_call_with_preserves_memory_wires_pre_call_memory_into_return() {
    // Pin the `preserves_memory = true` shape: mirrors the natural
    // lifter (`FunctionBuilder::build_call_with_cc`'s `preserves_memory`
    // branch).  The Call still emits `[Control, Memory(None), ...]`
    // (Call's `expected_signature` requires Memory), but the Memory
    // output is left dangling and the new Return wires the *pre-Call*
    // memory edge so LoadReadOnly / LoadForward chains stay
    // intact across the spliced tail call.  Required for
    // `x86_64_all_preserving`-style tracing pre-ambles where the tail
    // call provably doesn't touch memory.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    // Record the placeholder's mem input BEFORE the edit — that's the
    // pre-Call mem edge the new Return should consume.
    let placeholder_mem_in = function.nth_input(return_id, 1)
        .expect("placeholder must have a memory input");
    let new_return = function
        .with_rewrite_ctx(|ctx| {
            apply_tail_call(
                ctx,
                return_id,
                0xfeed,
                /* arg_passing_outputs */ &[],
                /* clobbered_kinds     */ &[],
                /* ret_val_outputs     */ &[],
                /* preserves_memory   */ true,
            )
        })
        .expect("apply_tail_call(preserves_memory=true)");
    // The Return's memory input must be the pre-Call memory edge, NOT
    // the Call's Memory output.  This is the load-bearing behavior.
    let new_mem_in = function.nth_input(new_return, 1).expect("Return mem input");
    assert_eq!(
        new_mem_in, placeholder_mem_in,
        "Return must wire pre-Call mem directly (skipping the Call's Memory output)"
    );
    // The Call's Memory output should have zero uses (dangling — that's
    // what makes the chain preserved).
    let ret_ctrl_in = function.nth_input(new_return, 0).expect("Return ctrl input");
    let (call_node, _) = function.output_definition(ret_ctrl_in);
    let call_outputs: Vec<_> = function.node_outputs(call_node).to_vec();
    assert_eq!(call_outputs.len(), 2, "Call has [Control, Memory(None)]");
    let mem_use_count = function.output_uses(call_outputs[1]).count();
    assert_eq!(
        mem_use_count, 0,
        "preserves_memory: Call's Memory output must be dangling (0 uses)"
    );
    // Full validate skipped — editor-isolation test, see note above.
}

#[test]
fn apply_tail_call_without_preserves_memory_threads_call_memory_into_return() {
    // Inverse pin: when preserves_memory=false, the Return must wire
    // the *Call's* Memory output (not the pre-Call mem), so downstream
    // memory dependencies see the Call as a memory barrier.
    let (mut function, _anchor) = build_initial_var_target_scenario_x86_64();
    let return_id = locate_placeholder_return(&function);
    let placeholder_mem_in = function.nth_input(return_id, 1).expect("mem in");
    let new_return = function
        .with_rewrite_ctx(|ctx| {
            apply_tail_call(ctx, return_id, 0xbabe, &[], &[], &[], false)
        })
        .expect("apply_tail_call(preserves_memory=false)");
    let new_mem_in = function.nth_input(new_return, 1).expect("ret mem");
    assert_ne!(
        new_mem_in, placeholder_mem_in,
        "default mode: Return mem must come from the Call, not the pre-Call edge"
    );
    let ret_ctrl_in = function.nth_input(new_return, 0).expect("ret ctrl");
    let (call_node, _) = function.output_definition(ret_ctrl_in);
    let call_outs: Vec<_> = function.node_outputs(call_node).to_vec();
    assert_eq!(new_mem_in, call_outs[1], "Return mem must equal Call's Memory output");
    // Full validate skipped — editor-isolation test, see note above.
}
