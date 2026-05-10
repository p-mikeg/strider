//! Black-box: `Kb::try_new` enforces the `ones & zeros == 0`
//! invariant that `Kb::merge` and `Kb::from_const` rely on.
//!
//! Direct struct-literal construction (`Kb { ones, zeros }`) bypasses
//! the check; `try_new` is the supported ctor for callers that
//! compute the masks themselves.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use opt::Kb;

#[test]
fn try_new_rejects_overlapping_bits() {
    let res = Kb::try_new(0xFF, 0xFF);
    assert!(res.is_err(), "0xFF/0xFF must be rejected");
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("ones & zeros must be 0"),
        "error should name the invariant; got {msg}"
    );
}

#[test]
fn try_new_accepts_disjoint_masks() {
    let kb = Kb::try_new(0x0F, 0xF0).expect("disjoint masks must construct");
    assert_eq!(kb.ones(), 0x0F);
    assert_eq!(kb.zeros(), 0xF0);
    assert_eq!(kb.ones() & kb.zeros(), 0);
}

#[test]
fn try_new_accepts_default_unknown() {
    let kb = Kb::try_new(0, 0).expect("(0, 0) is the canonical 'fully unknown'");
    assert_eq!(kb, Kb::default());
}

#[test]
fn try_new_rejects_partial_overlap() {
    // 0x07 ∩ 0x06 = 0x06 ≠ 0
    let res = Kb::try_new(0x07, 0x06);
    assert!(res.is_err());
}
