//! x87 `fyl2x` / `fyl2xp1` compute `ST1 * log2(ST0)` (plus one) through the
//! `log2` user op, so a function using them must lift rather than fail on an
//! unclassified op.

use crate::common;

const BASE: u64 = 0x1000;

#[test]
fn fyl2x_and_fyl2xp1_lift() {
    for (name, op) in [("fyl2x", [0xd9, 0xf1]), ("fyl2xp1", [0xd9, 0xf9])] {
        let bytes = vec![op[0], op[1], 0xc3];
        let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
        let result = strider.analyze(BASE, &cc, &Default::default(), None);
        assert!(result.is_ok(), "{name}: {:?}", result.err());
    }
}
