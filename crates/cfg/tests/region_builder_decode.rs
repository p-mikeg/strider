#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    // Two's-complement reinterpretation: CONST-space encodes signed pcode-offsets in a u64.
    clippy::cast_sign_loss,
)]

//! Tests for `RegionBuilder::decode_branch_target` —
//! CONST-relative / default-code-space-absolute / invalid-space-error paths.

mod common;
use common::{addr, fake_lift_res, fake_lift_res_with_len, make_builder, make_region_builder};

use cfg::ErrorKind;
use rsleigh::{Vn, VnAddr, VnSpace};

fn const_vn(offset: u64) -> Vn {
    Vn {
        addr: VnAddr { space: VnSpace::CONST, off: offset },
        size: 8,
    }
}

fn code_space_vn(space: VnSpace, offset: u64) -> Vn {
    Vn {
        addr: VnAddr { space, off: offset },
        size: 8,
    }
}

fn register_vn(offset: u64) -> Vn {
    Vn {
        addr: VnAddr { space: VnSpace::REGISTER, off: offset },
        size: 8,
    }
}

#[test]
fn const_space_is_relative_to_current_pcode_insn_index() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x2000, 0));
    // Target index will be 5; the lift_res must contain at least 6 pcode ops
    // for the new upper-bound check to accept it.
    let lift = fake_lift_res(8);
    let target = rb
        .decode_branch_target(const_vn(3), addr(0x2000, 2), &lift)
        .unwrap();
    // Expected: insn_index becomes current (2) + offset (3) = 5; machine_addr unchanged.
    assert_eq!(target, addr(0x2000, 5));
}

#[test]
fn const_space_with_zero_offset_stays_at_same_pcode_index() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x2000, 0));
    let lift = fake_lift_res(4);
    let target = rb
        .decode_branch_target(const_vn(0), addr(0x2000, 2), &lift)
        .unwrap();
    assert_eq!(target, addr(0x2000, 2));
}

#[test]
fn default_code_space_is_absolute_machine_address() {
    let mut b = make_builder(0x1000);
    let default_cs = cfg::test_api::sleigh(&b).default_code_space();
    let vn = code_space_vn(default_cs, 0xabc0);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    // Default-code-space arm doesn't read lift_res.insns; any non-empty fixture works.
    let lift = fake_lift_res(1);
    let target = rb
        .decode_branch_target(vn, addr(0x1000, 4), &lift)
        .unwrap();
    // Absolute: machine_addr = vn.off; insn_index = 0 regardless of caller's index.
    assert_eq!(target, addr(0xabc0, 0));
}

#[test]
fn register_space_returns_invalid_branch_target_error() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    let lift = fake_lift_res(1);
    let err = rb
        .decode_branch_target(register_vn(0x20), addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}

#[test]
fn unknown_space_returns_invalid_branch_target_error() {
    // A custom/unknown space byte is neither CONST nor default-code-space — should error.
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    let vn = Vn {
        addr: VnAddr { space: VnSpace::new(b'x'), off: 0x2000 },
        size: 8,
    };
    let lift = fake_lift_res(1);
    let err = rb
        .decode_branch_target(vn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}

#[test]
fn unique_space_returns_invalid_branch_target_error() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));
    let vn = Vn {
        addr: VnAddr { space: VnSpace::UNIQUE, off: 0x40 },
        size: 8,
    };
    let lift = fake_lift_res(1);
    let err = rb
        .decode_branch_target(vn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}

/// Pinned contract: a CONST-space relative branch target with a negative
/// offset (two's-complement `u64`) must decode to the predecessor pcode slot,
/// not wrap around `u64::MAX`.
#[test]
fn decode_branch_target_const_space_negative_offset_does_not_wrap() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));

    // Synthetic CONST varnode: off = -2 (two's complement u64), size irrelevant.
    let vn = Vn {
        addr: VnAddr {
            space: VnSpace::CONST,
            off: (-2_i64) as u64,
        },
        size: 8,
    };

    // We're currently at machine 0x1000, pcode index 5. A `-2` branch target
    // must decode to (0x1000, 3), not (0x1000, u64::MAX - 1).
    // Target index will be 3; lift_res must have at least 4 pcode ops.
    let lift = fake_lift_res(8);
    let got = rb
        .decode_branch_target(vn, addr(0x1000, 5), &lift)
        .unwrap();
    assert_eq!(got, addr(0x1000, 3));
}

/// Pinned contract: a CONST-space relative offset that would drive the
/// resulting pcode index negative must error rather than wrap.
#[test]
fn decode_branch_target_const_space_underflow_errors() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));

    let vn = Vn {
        addr: VnAddr {
            space: VnSpace::CONST,
            off: (-5_i64) as u64,
        },
        size: 8,
    };

    // At (0x1000, 2) with offset -5, the resulting index would be -3.
    let lift = fake_lift_res(8);
    let err = rb
        .decode_branch_target(vn, addr(0x1000, 2), &lift)
        .unwrap_err();
    assert!(matches!(
        err.kind(),
        ErrorKind::InvalidBranchTargetVaErr(_, _)
    ));
}

/// Pinned contract: a CONST-space relative branch target whose computed pcode
/// index lands past the last pcode op of the current machine instruction is
/// rejected at decode time. Without this check, the BUILD loop silently skips
/// the entry on its next pop and advances to the wrong machine instruction.
#[test]
fn decode_branch_target_const_space_index_past_end_errors() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));

    // Synthetic lift result with a known pcode count. The target index will be
    // `pcode_count + 1`, which is one past the last valid pcode slot.
    let pcode_count = 4u64;
    let lift = fake_lift_res(usize::try_from(pcode_count).expect("pcode_count fits in usize"));

    let vn = Vn {
        addr: VnAddr {
            space: VnSpace::CONST,
            off: pcode_count + 1, // forward jump past the last pcode op
        },
        size: 8,
    };

    let err = rb
        .decode_branch_target(vn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)),
        "expected InvalidBranchTargetVaErr; got {:?}",
        err.kind()
    );
}

/// Pinned contract: advancing past the last pcode op of the machine insn
/// at the very top of the address space returns
/// `ErrorKind::MachineAddrOverflow` rather than silently wrapping.
#[test]
fn next_pcode_addr_machine_address_overflow_errors() {
    // 1 pcode op, machine_insn_len = 16. The current addr is the only valid
    // pcode index, so `next_pcode_addr` advances by `machine_insn_len`. Place
    // the machine address near the top of the u64 range so the addition wraps.
    let lift = fake_lift_res_with_len(1, 16);
    let cur = addr(u64::MAX - 8, 0);
    let err = cfg::test_api::next_pcode_addr(cur, &lift).unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::MachineAddrOverflow(_)),
        "expected MachineAddrOverflow; got {:?}",
        err.kind()
    );
}

/// Sanity sibling: advancing past a non-saturating address still returns
/// `Ok(...)` after the signature change.
#[test]
fn next_pcode_addr_non_overflowing_advance_succeeds() {
    let lift = fake_lift_res_with_len(1, 4);
    let cur = addr(0x1000, 0);
    let next = cfg::test_api::next_pcode_addr(cur, &lift).unwrap();
    assert_eq!(next, addr(0x1004, 0));
}

/// Pinned contract: when `addr.insn_index + 1 < lift_res.insns.len()`,
/// `next_pcode_addr` increments `insn_index` and keeps `machine_addr`
/// unchanged. Covers the function's other (non-machine-advance) branch.
#[test]
fn next_pcode_addr_within_machine_insn_advances_pcode_index() {
    let lift = fake_lift_res(4);
    let cur = addr(0x1000, 1);
    let next = cfg::test_api::next_pcode_addr(cur, &lift).unwrap();
    assert_eq!(next, addr(0x1000, 2));
}

/// Regression for BUG-2: a CONST-space relative branch whose computed pcode
/// index equals `lift_res.insns.len()` (one-past-end) is the Sleigh
/// "fall-through to next machine instruction" idiom used by MIPS DIV / SLT.
/// It must decode to the first pcode op of the *next* machine instruction,
/// not produce an `InvalidBranchTargetVaErr`.
///
/// Previously the check was `target >= pcode_count`, which rejected this case.
/// The fix narrowed it to `target > pcode_count` and handles `==` specially.
#[test]
fn const_space_branch_to_pcode_count_falls_through_to_next_insn() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));

    // 4 pcode ops, machine_insn_len = 4 bytes.  Branch from index 0 with
    // offset +4 (== pcode_count) means "fall through to next machine insn".
    let pcode_count = 4usize;
    let lift = fake_lift_res_with_len(pcode_count, 4);

    let vn = Vn {
        addr: VnAddr {
            space: VnSpace::CONST,
            off: pcode_count as u64, // == insns.len() — the fall-through idiom
        },
        size: 8,
    };

    let target = rb
        .decode_branch_target(vn, addr(0x1000, 0), &lift)
        .unwrap();

    // Must advance to the next machine instruction (0x1000 + 4 = 0x1004),
    // pcode index 0 (first op of that machine insn).
    assert_eq!(
        target,
        addr(0x1004, 0),
        "fall-through branch must resolve to first pcode op of next machine insn"
    );
}

/// Pinned contract: a CONST-space relative branch target whose computed pcode
/// index is *strictly greater than* `lift_res.insns.len()` (more than one past
/// the last valid slot) must still be rejected as an invalid branch.
/// Only the exact `== pcode_count` case is the Sleigh fall-through idiom.
#[test]
fn decode_branch_target_const_space_index_past_pcode_count_errors() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));

    let pcode_count = 4u64;
    let lift = fake_lift_res(usize::try_from(pcode_count).expect("pcode_count fits in usize"));

    let vn = Vn {
        addr: VnAddr {
            space: VnSpace::CONST,
            off: pcode_count + 1, // strictly more than one past end — invalid
        },
        size: 8,
    };

    let err = rb
        .decode_branch_target(vn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}
