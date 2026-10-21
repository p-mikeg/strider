//! Each x87 `fcmovcc st0, st1` moves exactly when its EFLAGS condition holds
//! (SDM Vol. 2A FCMOVcc): `fcmovnbe` moves iff CF=0 and ZF=0.
//!
//! The flags come from `cmp eax, 2`, and every case was also run natively on
//! x86-64 from the same bytes, agreeing with the predicates below.

mod common;

const BASE: u64 = 0x1000;

/// `fild 7` / `fild 1` (so st0 = 1, st1 = 7) / `mov eax, a` / `cmp eax, b` /
/// the fcmov / `fistp dword [rsp-16]` / `fstp st0` / `mov eax, [rsp-16]` /
/// `ret`.
fn image(op: [u8; 2], a: u32, b: u32) -> Vec<u8> {
    let mut v = vec![0xc7, 0x44, 0x24, 0xf8, 0x07, 0x00, 0x00, 0x00];
    v.extend_from_slice(&[0xdb, 0x44, 0x24, 0xf8]);
    v.extend_from_slice(&[0xc7, 0x44, 0x24, 0xf8, 0x01, 0x00, 0x00, 0x00]);
    v.extend_from_slice(&[0xdb, 0x44, 0x24, 0xf8]);
    v.push(0xb8);
    v.extend_from_slice(&a.to_le_bytes());
    v.push(0x3d);
    v.extend_from_slice(&b.to_le_bytes());
    v.extend_from_slice(&op);
    v.extend_from_slice(&[
        0xdb, 0x5c, 0x24, 0xf0, 0xdd, 0xd8, 0x8b, 0x44, 0x24, 0xf0, 0xc3,
    ]);
    v
}

/// `(CF, ZF, PF)` after `cmp a, b`.
fn flags(a: u32, b: u32) -> (bool, bool, bool) {
    let r = a.wrapping_sub(b);
    (a < b, r == 0, (r & 0xff).count_ones().is_multiple_of(2))
}

type Predicate = fn(bool, bool, bool) -> bool;

const FCMOV: [(&str, [u8; 2], Predicate); 8] = [
    ("fcmovb", [0xda, 0xc1], |cf, _, _| cf),
    ("fcmove", [0xda, 0xc9], |_, zf, _| zf),
    ("fcmovbe", [0xda, 0xd1], |cf, zf, _| cf || zf),
    ("fcmovu", [0xda, 0xd9], |_, _, pf| pf),
    ("fcmovnb", [0xdb, 0xc1], |cf, _, _| !cf),
    ("fcmovne", [0xdb, 0xc9], |_, zf, _| !zf),
    ("fcmovnbe", [0xdb, 0xd1], |cf, zf, _| !cf && !zf),
    ("fcmovnu", [0xdb, 0xd9], |_, _, pf| !pf),
];

#[test]
fn each_fcmov_moves_exactly_when_its_condition_holds() {
    for (name, op, moves) in FCMOV {
        // CF only, ZF only, neither (PF clear), CF with PF clear, neither.
        for (a, b) in [(1, 2), (2, 2), (3, 2), (0, 2), (5, 2)] {
            let (cf, zf, pf) = flags(a, b);
            let want = if moves(cf, zf, pf) { 7 } else { 1 };
            let got = common::cone::returned_rax(image(op, a, b), BASE) as u32;
            assert_eq!(
                got, want,
                "{name} after cmp {a}, {b} (CF {cf} ZF {zf} PF {pf})"
            );
        }
    }
}
