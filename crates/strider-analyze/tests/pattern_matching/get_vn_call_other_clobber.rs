//! `Match::get_vn` resolves clobber-slot bindings to the source
//! [`rsleigh::Vn`] for both `Call` and `CallOther` producers.
//!
//! - For `Call`, the per-call override on
//!   [`strider_ir::Graph::call_clobbered_override`] shadows the
//!   function-default [`strider_ir::Graph::call_clobbered_regs`].
//! - For `CallOther` (emitted by `build_call_other_modeled`), the
//!   per-CallOther override is always recorded when `implicit_writes_vns`
//!   is non-empty, so the production path through `get_vn` always reads
//!   the override list.  The value output (when present) does not map
//!   to a varnode.

use strider_analyze::pattern::{Capture, Matcher, call, call_other, var};
use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};
use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

// ── Call: build_call_with_cc records per-call override ───────────────────────

#[test]
fn build_call_with_cc_override_records_empty_clobber_list() {
    // Every tracked variable is callee-saved in the override CC →
    // per-call clobber = ∅ → 0 clobber output slots and an empty
    // override list on the side-table.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();

    let cc = CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    // `FunctionBuilder::new` auto-tracks ret-val regs (rax, rdx, xmm0, xmm1).
    let rdx = regs.name_to_vn("RDX").unwrap();
    let xmm0 = regs.name_to_vn("XMM0").unwrap();
    let xmm1 = regs.name_to_vn("XMM1").unwrap();
    let override_cc = BuiltCallingConvention::try_new(
        vec![],                          // arg_passing_regs
        vec![rax, rdx, xmm0, xmm1],      // callee_saved_regs (every tracked var)
        vec![],                          // ret_val_regs
        vec![],                          // ret_val_regs_float
        rsp,                             // stack_vn
        vec![],                          // stack_arg_offsets
        0,                               // ret_stack_pop
        None,                            // link_register_vn
        false,                           // no_memory_clobber
    )
    .unwrap();
    let addr = b
        .build_int_const(0xdead_u64, NodeOutputType::I64)
        .unwrap();
    let _call_node = b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    let ret_vars: Vec<rsleigh::Vn> = b.ret_val_vars().to_vec();
    b.build_return(None, &ret_vars).unwrap();
    let function = b.build().unwrap();

    // The single Call has 0 clobber outputs (ctrl + mem only) and the
    // side-table records an empty override list.
    let call_id = function
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(function.call_clobbered_override(call_id), Some(&[][..]));
    assert_eq!(function.node_outputs(call_id).len(), 2);
}

// ── CallOther: implicit_writes records override, get_vn reads it ─────────────

#[test]
fn get_vn_for_callother_clobber_slot_uses_override_list() {
    // CallOther with implicit_writes_vns=[rax] produces a clobber output
    // at slot 2 and records `[rax]` on the per-CallOther override.
    // `get_vn` on slot 2 must return rax.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let rax_value = b.read_variable(&rax).unwrap();
    let (_node, _value, _clobbers) = b
        .build_call_other_modeled(
            7,
            "syscall_like",
            &[],
            None,
            &[rax_value],
            &[rax],
            &[NodeOutputKind::OutputType(NodeOutputType::I64)],
        )
        .expect("call_other_modeled");
    b.build_return(None, &[rax]).expect("ret");
    let function = b.build().expect("build");

    let c = Capture::new();
    // CallOther outputs: [ctrl(0), mem(1), clobber_rax(2)].  ret(2, ...)
    // captures the clobber output.
    let pat = call_other().name("syscall_like").ret(2, var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat.into());
    assert_eq!(hits.len(), 1, "CallOther should match exactly once");
    assert_eq!(
        hits[0].get_vn(c, &function),
        Some(rax),
        "get_vn on the rax clobber slot returns rax"
    );
}

#[test]
fn get_vn_for_callother_value_output_returns_none() {
    // CallOther with a value output at slot 2 and a clobber at slot 3.
    // get_vn on the value slot has no varnode mapping; on the clobber
    // slot it returns the override-list entry.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let rax_value = b.read_variable(&rax).unwrap();
    let (_node, _value, _clobbers) = b
        .build_call_other_modeled(
            7,
            "valued_op",
            &[],
            Some(NodeOutputType::I32), // value output at slot 2
            &[rax_value],
            &[rax],                                          // clobber output at slot 3
            &[NodeOutputKind::OutputType(NodeOutputType::I64)],
        )
        .expect("call_other_modeled with value");
    b.build_return(None, &[rax]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::try_new(&function).unwrap();

    // Slot 3 (clobber) → rax.
    let c_clob = Capture::new();
    let hits_clob = m.find_all(&call_other().name("valued_op").ret(3, var(c_clob)).into());
    assert_eq!(hits_clob.len(), 1);
    assert_eq!(hits_clob[0].get_vn(c_clob, &function), Some(rax));

    // Slot 2 (value) → None (no varnode mapping for the user-op's value).
    let c_val = Capture::new();
    let hits_val = m.find_all(&call_other().name("valued_op").ret(2, var(c_val)).into());
    assert_eq!(hits_val.len(), 1);
    assert_eq!(
        hits_val[0].get_vn(c_val, &function),
        None,
        "value output of a CallOther has no varnode mapping",
    );
}

#[test]
fn get_vn_callother_override_shadows_function_default() {
    // Function-default `call_other_clobbered` is `[rax]` (every tracked
    // var minus SP).  The CallOther's implicit_writes_vns=[rbx] records
    // `[rbx]` on the per-CallOther override.  get_vn on slot 2 must
    // return rbx (the override), not rax (the function-default).
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();

    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .tracked(rbx)
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let rbx_value = b.read_variable(&rbx).unwrap();
    let (_node, _value, _clobbers) = b
        .build_call_other_modeled(
            7,
            "rbx_clobber",
            &[],
            None,
            &[rbx_value],
            &[rbx],                                          // override list
            &[NodeOutputKind::OutputType(NodeOutputType::I64)],
        )
        .expect("call_other_modeled");
    b.build_return(None, &[rax, rbx]).expect("ret");
    let function = b.build().expect("build");

    // Sanity: the function-default has rax first.
    assert!(
        function.call_other_clobbered_regs().contains(&rax),
        "function-default call_other_clobbered should include rax"
    );

    let c = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&call_other().name("rbx_clobber").ret(2, var(c)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].get_vn(c, &function),
        Some(rbx),
        "per-CallOther override must shadow function-default",
    );
}

// ── Call: function-default fallback (no override) ────────────────────────────

#[test]
fn get_vn_for_call_clobber_slot_uses_function_default() {
    // `build_call` (no override_cc) creates clobber outputs from the
    // function-default `call_clobbered_variables` and does NOT record an
    // override.  `get_vn` on slot 2 falls back to
    // `Graph::call_clobbered_regs()[0]`.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    // Tracked = [rax] only, no SP.  rax is not callee-saved, so it
    // becomes the single function-default clobber.
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr = b
        .build_int_const(0xdead_u64, NodeOutputType::I64)
        .unwrap();
    b.build_call(addr).expect("build_call");
    b.build_return(None, &[rax]).expect("ret");
    let function = b.build().expect("build");

    // Sanity: no per-Call override recorded.
    let call_id = function
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(function.call_clobbered_override(call_id), None);
    assert_eq!(function.call_clobbered_regs(), &[rax][..]);

    // Pattern: capture the Call's clobber slot 0 (=slot 2).  get_vn
    // returns rax via the function-default fallback.
    let c = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&call().at(0xdead_u64).ret_output(0, var(c)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_vn(c, &function), Some(rax));
}

// ── Call: per-Call override shadows function-default ─────────────────────────

#[test]
fn get_vn_for_call_clobber_slot_uses_override_when_set() {
    // A plain `Call` (no `_with_cc` override at build time) whose
    // per-Call clobber override is set explicitly via
    // `set_call_clobbered_override`.  The override must shadow the
    // function-default `call_clobbered_regs` at `get_vn` time.
    //
    // Pin against A9-M3: the orchestrator stamps overrides after
    // splicing tail-call edits, so pattern queries against those
    // spliced Calls must read the override list, not the function
    // default.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();

    // Function-default: tracked = [rax].  rax becomes the single
    // function-default clobber.
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr = b.build_int_const(0xdead_u64, NodeOutputType::I64).unwrap();
    b.build_call(addr).expect("build_call");
    b.build_return(None, &[rax]).expect("ret");
    let mut function = b.build().expect("build");
    // Manually stamp the per-Call override on the side-table after
    // build.  The override list `[rbx]` differs from the
    // function-default `[rax]` and includes a register that isn't
    // even tracked — `get_vn` must still resolve it.
    let call_node = function
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .expect("Call must exist");
    function.set_call_clobbered_override(call_node, vec![rbx]);

    // Sanity: function-default has rax; the override has rbx.
    assert_eq!(function.call_clobbered_regs(), &[rax][..]);
    let call_id = function
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(function.call_clobbered_override(call_id), Some(&[rbx][..]));

    // get_vn on slot 2 (the first clobber output) reads the override.
    let c = Capture::new();
    let hits = Matcher::try_new(&function)
        .unwrap()
        .find_all(&call().at(0xdead_u64).ret_output(0, var(c)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].get_vn(c, &function),
        Some(rbx),
        "per-Call override must shadow function-default at get_vn time"
    );
}
