#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `RegionBuilder::decode_branch_target` —
//! CONST-relative / default-code-space-absolute / invalid-space-error paths.

mod common;
use common::{addr, fake_lift_res, make_builder, make_region_builder};

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
    let lift = fake_lift_res(pcode_count as usize);

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

/// Pinned contract: a CONST-space relative branch target whose computed pcode
/// index equals `lift_res.insns.len()` (the first index past the last valid
/// slot) must also be rejected — the bound is exclusive.
#[test]
fn decode_branch_target_const_space_index_at_end_errors() {
    let mut b = make_builder(0x1000);
    let rb = make_region_builder(&mut b, addr(0x1000, 0));

    let pcode_count = 4u64;
    let lift = fake_lift_res(pcode_count as usize);

    let vn = Vn {
        addr: VnAddr {
            space: VnSpace::CONST,
            off: pcode_count, // exactly one past the last valid index
        },
        size: 8,
    };

    let err = rb
        .decode_branch_target(vn, addr(0x1000, 0), &lift)
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}
