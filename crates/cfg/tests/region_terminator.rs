#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Tests for `RegionTerminator` — the per-region terminator enum that
//! replaces the old `Region::ends_with_tail_call: bool`.
//!
//! Coverage targets one positive test per `RegionTerminator` variant
//! currently produced by the cfg builder, plus the legacy `BranchIndirect ->
//! Return` mapping (will be replaced when the indirect-branch resolver lands)
//! and a sanity check on the not-yet-constructed `Switch` variant.

mod common;
use common::{
    addr, je_rel8_ret_ret_bytes, jmp_rax_bytes, jmp_rel8_ret_bytes, make_builder_with_bytes,
    make_region, make_region_builder, make_sleigh_with_bytes, ret_bytes,
};

use cfg::test_api::{self, ProcessInsnRes, Region, RegionInstruction};
use cfg::{Builder, OptionsBuilder, RegionTerminator};

/// Lift one x86-64 machine instruction at `at` from `bytes` starting at `base`.
fn lift_at(bytes: Vec<u8>, base: u64, at: u64) -> rsleigh::LiftRes {
    make_sleigh_with_bytes(bytes, base)
        .lift_one(at)
        .expect("lift_one")
}

/// Locate the first pcode insn in `lift` whose opcode matches `want`.
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

#[test]
fn finish_with_branch_terminator() {
    // `jmp +1` — non-tail-call, non-fallthrough Branch.  Target 0x1003 is
    // distinct from the natural fall-through 0x1002, so the BUG-25
    // normalisation does not kick in and the edge stays `Branch`.
    let base = 0x1000u64;
    let bytes = jmp_rel8_ret_bytes(1);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&branch, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].terminator, RegionTerminator::Branch);
}

#[test]
fn finish_with_cond_branch_terminator() {
    // `je +0; ret; ret` — the `je` lifts to a CondBranch.
    let base = 0x1000u64;
    let bytes = je_rel8_ret_ret_bytes(0);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, cbr) = find_pcode(&lift, rsleigh::Opcode::CondBranch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&cbr, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].terminator, RegionTerminator::CondBranch);
}

#[test]
fn finish_with_return_terminator() {
    // `ret` — single-byte x86-64.
    let base = 0x1000u64;
    let bytes = ret_bytes();
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, ret_insn) = find_pcode(&lift, rsleigh::Opcode::Return);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb
        .process_new_insn(&ret_insn, addr(base, pos), &lift)
        .unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].terminator, RegionTerminator::Return);
}

#[test]
fn finish_with_tail_call_terminator() {
    // `jmp -10` from 0x1000 — the rel8 displacement is relative to pc +
    // insn_len = 0x1002, so the resolved target is 0x0ff8.  That target
    // lies below the function start at 0x1000 and is therefore classified
    // as a tail call.
    let base = 0x1000u64;
    let bytes = jmp_rel8_ret_bytes(-10);
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, branch) = find_pcode(&lift, rsleigh::Opcode::Branch);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb.process_new_insn(&branch, addr(base, pos), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    // The work queue must remain untouched (tail-call does not enqueue a
    // successor) and the resulting region's terminator must carry the
    // resolved target address.
    assert_eq!(test_api::work_queue(&b).len(), 0);
    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(
        regions[0].terminator,
        RegionTerminator::TailCall { target: 0x0ff8 }
    );
}

#[test]
fn finish_with_fallthrough_terminator() {
    // Drive `process_insn` at an address that already starts a known region:
    // the current region is closed out as a Fallthrough into the existing
    // region, with `Region::terminator == Fallthrough`.
    let base = 0x1000u64;
    let bytes = vec![0x31u8, 0xc0, 0xc3]; // xor eax,eax; ret
    let lift = lift_at(bytes.clone(), base, base);
    assert!(!lift.insns.is_empty());
    let mut b = make_builder_with_bytes(bytes, base);

    let _existing = test_api::add_region(&mut b, make_region(&[(0x1004, 0)])).unwrap();

    let mut rb = make_region_builder(&mut b, addr(base, 0));
    rb.push_insn(RegionInstruction {
        addr: addr(base, 0),
        insn: lift.insns[0].clone(),
    });

    let dummy = lift.insns[0].clone();
    let res = rb.process_insn(&dummy, addr(0x1004, 0), &lift).unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    // Two regions: the pre-existing one at 0x1004 (Fallthrough — its
    // contents are placeholder fake_insns from `make_region` which the
    // Fallthrough constructor flagged) and the just-finished current
    // region whose terminator must be `Fallthrough`.
    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 2);
    let saw_fallthrough = regions
        .iter()
        .any(|r| r.terminator == RegionTerminator::Fallthrough);
    assert!(saw_fallthrough, "at least one region must be Fallthrough");
}

#[test]
fn split_first_half_becomes_fallthrough() {
    // `xor eax,eax; xor eax,eax; jmp -4` — the back-jump targets 0x1002
    // mid-region, triggering split_region.  Post-split:
    //   first half  (0x1000..0x1002) — terminator = Fallthrough
    //   second half (0x1002..)       — inherits the original terminator
    //                                  (here: Branch, because the back-jump's
    //                                  target is 0x1002 which is NOT the
    //                                  natural fallthrough 0x1006).
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let cfg = Builder::new(
        make_sleigh_with_bytes(bytes, 0x1000),
        0x1000,
        OptionsBuilder::new().build(),
    )
    .build()
    .unwrap();

    let mut first_half: Option<&Region> = None;
    let mut second_half: Option<&Region> = None;
    for r in cfg.graph.node_weights() {
        if r.start_addr.machine_addr.addr == 0x1000 {
            first_half = Some(r);
        } else if r.start_addr.machine_addr.addr == 0x1002 {
            second_half = Some(r);
        }
    }
    let first_half = first_half.expect("first half (0x1000) region");
    let second_half = second_half.expect("second half (0x1002) region");

    assert_eq!(
        first_half.terminator,
        RegionTerminator::Fallthrough,
        "first half of a split region must be Fallthrough"
    );
    assert_eq!(
        second_half.terminator,
        RegionTerminator::Branch,
        "second half must inherit the original region's terminator (Branch from the back-jump)"
    );
}

#[test]
fn branch_indirect_currently_terminates_as_return() {
    // Phase 3 is a pure refactor — `BranchIndirect` keeps the legacy
    // `Return` mapping until Phase 5 wires up the resolver.  This pin
    // catches accidental behaviour shifts in the meantime.
    let base = 0x1000u64;
    let bytes = jmp_rax_bytes();
    let lift = lift_at(bytes.clone(), base, base);
    let (pos, indirect_insn) = find_pcode(&lift, rsleigh::Opcode::BranchIndirect);
    let mut b = make_builder_with_bytes(bytes, base);
    let mut rb = make_region_builder(&mut b, addr(base, 0));

    let res = rb
        .process_new_insn(&indirect_insn, addr(base, pos), &lift)
        .unwrap();
    assert_eq!(res, ProcessInsnRes::FinishedProcessing);

    let regions: Vec<&Region> = test_api::graph(&b).node_weights().collect();
    assert_eq!(regions.len(), 1);
    assert_eq!(
        regions[0].terminator,
        RegionTerminator::Return,
        "BranchIndirect maps to Return until Phase 5's resolver lands"
    );
}

#[test]
fn switch_variant_is_constructible_but_unused() {
    // The `Switch` variant is reserved for the future jump-table resolver;
    // pin its API shape now so future construction is purely additive.
    let term = RegionTerminator::Switch {
        targets: Vec::new(),
    };
    let cloned = term.clone();
    assert_eq!(term, cloned);
    match term {
        RegionTerminator::Switch { targets } => {
            assert!(targets.is_empty());
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}
