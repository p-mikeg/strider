#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `RegionBuilder::process_new_insn`, `RegionBuilder::process_insn`,
//! and `RegionBuilder::finish_current_region`.

mod common;
use common::{
    addr, je_rel8_ret_ret_bytes, jmp_rax_bytes, jmp_rel8_ret_bytes, make_builder,
    make_builder_with_bytes, make_region, make_region_builder, make_sleigh_with_bytes, ret_bytes,
};

use cfg::test_api::{self, ProcessInsnRes, RegionInstruction};
use cfg::{ErrorKind, RegionEdgeKind, RegionTerminator};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};

/// Lift one x86-64 machine instruction at `at` from `bytes` starting at `base`.
/// Creates a temporary sleigh independent of any `Builder`.
fn lift_at(bytes: Vec<u8>, base: u64, at: u64) -> rsleigh::LiftRes {
    make_sleigh_with_bytes(bytes, base)
        .lift_one(at)
        .expect("lift_one")
}

/// Finds the first pcode insn in `lift` whose opcode matches `want`, returns
/// (`insn_index`, insn clone).
#[allow(clippy::panic)]
fn find_pcode(lift: &rsleigh::LiftRes, want: rsleigh::Opcode) -> (u64, rsleigh::Insn) {
    let (idx, i) = lift
        .insns
        .iter()
        .enumerate()
        .find(|(_, i)| i.opcode == want)
        .unwrap_or_else(|| panic!("no pcode op with opcode {want:?}"));
    (idx as u64, i.clone())
}

// ── process_new_insn ──────────────────────────────────────────────────────────

#[test]
fn non_terminating_insn_keeps_region_open() {
    // `xor eax, eax` (0x31 0xc0) expands to pcode ops that are non-terminating.
    // NOP (0x90) produces zero pcode ops in x86-64 Sleigh, so we use xor instead.
    let base = 0x1000u64;
    let bytes = vec![0x31u8, 0xc0]; // xor eax, eax
    let lift = lift_at(bytes.clone(), base, base);
    assert!(!lift.insns.is_empty(), "xor eax,eax must produce at least one pcode op");
    let first = lift.insns[0].clone();
    assert!(
        !matches!(first.opcode, rsleigh::Opcode::Branch | rsleigh::Opcode::CondBranch | rsleigh::Opcode::Return),
        "test assumption: first pcode op of `xor eax,eax` is non-terminating"
    );
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&first, addr(base, 0), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::DidntFinishProcessing);
    assert_eq!(rb.insns().len(), 1);
}

#[test]
fn return_ends_region() {
    let base = 0x1000u64;
    let bytes = ret_bytes();
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, ret_insn) = find_pcode(&lift, rsleigh::Opcode::Return);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&ret_insn, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);
}

/// BUG-5 regression: `BranchIndirect` (e.g. ARM `bx lr` return, x86 `jmp rax`)
/// must terminate the region.  Pre-fix, BranchIndirect fell into the catch-
/// all "didn't finish processing" branch and the CFG builder kept walking
/// past the function boundary, absorbing adjacent code.
#[test]
fn branch_indirect_ends_region() {
    let base = 0x1000u64;
    let bytes = jmp_rax_bytes();
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, indirect_insn) = find_pcode(&lift, rsleigh::Opcode::BranchIndirect);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb
        .process_new_insn(&indirect_insn, addr(base, pos), &lift)
        .unwrap();
    assert_eq!(
        res,
        ProcessInsnRes::FinishedProcessing,
        "BranchIndirect must terminate the region — without this the CFG \
         builder walks past the function boundary"
    );
}

#[test]
fn branch_non_tail_enqueues_target() {
    // `jmp +0` at 0x1000 — target is 0x1002 (absolute, via default code space).
    // 0x1002 is exactly `pc + insn_len`, i.e. the next instruction, so per
    // BUG-25 normalisation the edge is classified as `Fallthrough`.  See
    // `branch_non_fallthrough_target_keeps_branch_edge` below for the
    // "real" non-fallthrough case.
    let base = 0x1000u64;
    let bytes = jmp_rel8_ret_bytes(0);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&branch, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    assert_eq!(test_api::work_queue(&b).len(), 1);
    let (parent, enqueued_addr) = test_api::work_queue(&b)[0];
    assert_eq!(enqueued_addr, addr(0x1002, 0));
    let (_, kind) = parent.expect("branch must have a parent edge");
    assert_eq!(kind, RegionEdgeKind::Fallthrough);
}

/// BUG-25 regression: a non-tail-call `Branch` whose target is *not* the
/// next instruction must keep the `Branch` edge kind.  Here the branch
/// goes forward by one byte (target 0x1003 = pc + 3, but the `jmp` itself
/// is only 2 bytes long, so `pc + insn_len = 0x1002`).  0x1003 != 0x1002
/// so the edge stays `Branch`.
#[test]
fn branch_non_fallthrough_target_keeps_branch_edge() {
    // `jmp +1` at 0x1000 — target is 0x1003 (the `ret` byte's *next* slot,
    // not 0x1002 which would be the natural fallthrough).
    let base = 0x1000u64;
    let bytes = jmp_rel8_ret_bytes(1);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&branch, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    assert_eq!(test_api::work_queue(&b).len(), 1);
    let (parent, enqueued_addr) = test_api::work_queue(&b)[0];
    assert_eq!(enqueued_addr, addr(0x1003, 0));
    let (_, kind) = parent.expect("branch must have a parent edge");
    assert_eq!(kind, RegionEdgeKind::Branch);
}

#[test]
fn branch_tail_call_sets_ends_with_tail_call_flag_and_does_not_enqueue() {
    // `jmp -10` from 0x1000 — the rel8 displacement is relative to the
    // *next* instruction (pc + insn_len = 0x1002), so the resolved target
    // is 0x0ff8, which lies below the function start at 0x1000 and is
    // therefore classified as a tail call.
    let base = 0x1000u64;
    let bytes = jmp_rel8_ret_bytes(-10);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&branch, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    // Queue untouched — tail call doesn't enqueue the target.
    assert_eq!(test_api::work_queue(&b).len(), 0);
    // Exactly one region was added with terminator = TailCall { target }.
    let regions: Vec<_> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(
        regions[0].terminator,
        RegionTerminator::TailCall { target: 0x0ff8 }
    );
}

#[test]
fn cond_branch_enqueues_both_cases() {
    // `je +0; ret; ret` — conditional short jump.
    let base = 0x1000u64;
    let bytes = je_rel8_ret_ret_bytes(0);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, cbr) = find_pcode(&lift, rsleigh::Opcode::CondBranch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&cbr, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    let queue: Vec<_> = test_api::work_queue(&b).to_vec();
    assert_eq!(queue.len(), 2, "CondBranch must enqueue both true and false targets");

    // Exactly one IfCaseTrue and one IfCaseFalse were enqueued.
    let mut kinds: Vec<RegionEdgeKind> = queue
        .iter()
        .filter_map(|(parent, _)| parent.as_ref().map(|(_, k)| *k))
        .collect();
    kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(
        kinds,
        vec![RegionEdgeKind::IfCaseFalse, RegionEdgeKind::IfCaseTrue]
    );
}

// ── process_insn ──────────────────────────────────────────────────────────────

#[test]
fn process_insn_falls_through_into_existing_region_start() {
    // Pre-register a region at 0x1004. Then drive `process_insn` at 0x1004 —
    // it should close out the current region with a Fallthrough edge into
    // the existing one, without decoding the passed-in insn body.
    // Use `xor eax,eax; ret` so the lift of the first machine insn has pcode ops.
    // NOP (0x90) produces zero pcode ops in x86-64 Sleigh.
    let base = 0x1000u64;
    let bytes = vec![0x31u8, 0xc0, 0xc3]; // xor eax,eax; ret
    let lift = lift_at(bytes.clone(), base, base);
    assert!(!lift.insns.is_empty(), "xor eax,eax must produce at least one pcode op");
    let mut b = make_builder_with_bytes(bytes, base);

    let existing = test_api::add_region(&mut b, make_region(&[(0x1004, 0)])).unwrap();

    // Build a RegionBuilder that has already consumed one insn so
    // finish_current_region has something to close.
    let mut rb = make_region_builder(&mut b, addr(base, 0));
    rb.push_insn(RegionInstruction { addr: addr(base, 0), insn: lift.insns[0].clone() });

    // Call process_insn at the existing-start addr; the insn body is irrelevant
    // because the addr-match path is taken first.
    let dummy = lift.insns[0].clone();
    let res = rb.process_insn(&dummy, addr(0x1004, 0), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    // Exactly one Fallthrough edge into the existing region.
    let ft_count = test_api::graph(&b)
        .edge_references()
        .filter(|e| *e.weight() == RegionEdgeKind::Fallthrough && e.target() == existing)
        .count();
    assert_eq!(ft_count, 1);
}

// ── finish_current_region ────────────────────────────────────────────────────

#[test]
fn finish_current_region_empty_insns_returns_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb
        .finish_current_region(RegionTerminator::Return)
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::EmptyRegion(_)));
}

// ── empty-inputs branch / condbranch rejection ──────────────────────────────

/// Pinned contract: a `Branch` pcode instruction with empty inputs is
/// rejected with `MissingBranchTarget` rather than panicking on
/// `insn.inputs[0]`.
#[test]
fn process_new_insn_branch_with_empty_inputs_errors() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    // The `lift_res` content doesn't matter — `process_new_insn`'s Branch
    // arm errors before reading `lift_res`.
    let lift = common::fake_lift_res(1);

    let bad_insn = rsleigh::Insn {
        opcode: rsleigh::Opcode::Branch,
        inputs: vec![],
        output: None,
    };

    let err = rb
        .process_new_insn(&bad_insn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::MissingBranchTarget(_)),
        "expected MissingBranchTarget; got {:?}",
        err.kind()
    );
}

/// Symmetric pinned contract for `CondBranch`.
#[test]
fn process_new_insn_condbranch_with_empty_inputs_errors() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let lift = common::fake_lift_res(1);

    let bad_insn = rsleigh::Insn {
        opcode: rsleigh::Opcode::CondBranch,
        inputs: vec![],
        output: None,
    };

    let err = rb
        .process_new_insn(&bad_insn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::MissingBranchTarget(_)),
        "expected MissingBranchTarget; got {:?}",
        err.kind()
    );
}
