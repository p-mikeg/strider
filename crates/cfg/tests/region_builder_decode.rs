#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `RegionBuilder::decode_branch_target` —
//! CONST-relative / default-code-space-absolute / invalid-space-error paths.

mod common;
use common::{addr, make_builder, make_region_builder};

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
    let mut rb = make_region_builder(&mut b, addr(0x2000, 0));
    let target = rb.decode_branch_target(const_vn(3), addr(0x2000, 2)).unwrap();
    // Expected: insn_index becomes current (2) + offset (3) = 5; machine_addr unchanged.
    assert_eq!(target, addr(0x2000, 5));
}

#[test]
fn const_space_with_zero_offset_stays_at_same_pcode_index() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x2000, 0));
    let target = rb.decode_branch_target(const_vn(0), addr(0x2000, 2)).unwrap();
    assert_eq!(target, addr(0x2000, 2));
}

#[test]
fn default_code_space_is_absolute_machine_address() {
    let mut b = make_builder(0x1000);
    let default_cs = cfg::test_api::sleigh(&b).default_code_space();
    let vn = code_space_vn(default_cs, 0xabc0);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let target = rb.decode_branch_target(vn, addr(0x1000, 4)).unwrap();
    // Absolute: machine_addr = vn.off; insn_index = 0 regardless of caller's index.
    assert_eq!(target, addr(0xabc0, 0));
}

#[test]
fn register_space_returns_invalid_branch_target_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let err = rb.decode_branch_target(register_vn(0x20), addr(0x1000, 0)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}

#[test]
fn unknown_space_returns_invalid_branch_target_error() {
    // A custom/unknown space byte is neither CONST nor default-code-space — should error.
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let vn = Vn {
        addr: VnAddr { space: VnSpace::new(b'x'), off: 0x2000 },
        size: 8,
    };
    let err = rb.decode_branch_target(vn, addr(0x1000, 0)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}

#[test]
fn unique_space_returns_invalid_branch_target_error() {
    let mut b = make_builder(0x1000);
    let mut rb = make_region_builder(&mut b, addr(0x1000, 0));
    let vn = Vn {
        addr: VnAddr { space: VnSpace::UNIQUE, off: 0x40 },
        size: 8,
    };
    let err = rb.decode_branch_target(vn, addr(0x1000, 0)).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidBranchTargetVaErr(_, _)));
}
