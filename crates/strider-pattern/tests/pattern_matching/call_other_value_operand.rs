//! `CallOther` / `Call` nested as a **value** operand of another node, e.g.
//! `add(x, call_other().name("f"))` — matching an arithmetic node one of whose
//! operands is a value output of the call. Loose by default: any value output
//! of the matching call satisfies the operand (the `.out(i)` selector, tested
//! separately, pins a specific one).

use strider_ir::Function;
use strider_ir_test_utils::{Tb, reg_vn};
use strider_pattern::{Matcher, MatchPat, add, any, call_other, int_const};

/// `getval()` (a user-op producing a value in an I64 register) feeding
/// `add(getval(), 5)`, returned so the `add` is reachable.
fn call_other_feeds_add() -> Function {
    let out = reg_vn(0x10, 8); // I64 result register
    let mut t = Tb::with_vars(&[out]);
    let ret = t
        .call_other("getval", 42, &[], Some(out), &[], &[])
        .expect("getval has a value output");
    let k = t.u64(5);
    let sum = t.add(ret, k);
    t.ret_val(sum)
}

#[test]
fn call_other_nests_as_value_operand() {
    let f = call_other_feeds_add();
    let m = Matcher::new(&f);
    // add whose operand is a value output of the CallOther named "getval".
    assert_eq!(
        m.find_all(&add(any(), call_other().name("getval")).into_pattern())
            .unwrap()
            .len(),
        1,
    );
    // Wrong user-op name → no match.
    assert_eq!(
        m.find_all(&add(any(), call_other().name("nope")).into_pattern())
            .unwrap()
            .len(),
        0,
    );
    // The other operand is still matchable alongside it.
    assert_eq!(
        m.find_all(&add(int_const(5u128), call_other().name("getval")).into_pattern())
            .unwrap()
            .len(),
        1,
    );
}

/// `getval()` producing a **result** (I64 reg) plus a **clobber** (another I64
/// reg via implicit-write), each feeding its own `add`. Both adds are kept
/// reachable by summing them into the return value.
fn call_other_result_and_clobber_feed_adds() -> Function {
    let res = reg_vn(0x10, 8);
    let clob = reg_vn(0x20, 8);
    let mut t = Tb::with_vars(&[res, clob]);
    let result = t
        .call_other("getval", 42, &[], Some(res), &[], &[clob])
        .expect("getval has a result output");
    let clobber = t.read_var(&clob); // the CallOther's clobber value output
    let k1 = t.u64(5);
    let a_res = t.add(result, k1);
    let k2 = t.u64(7);
    let a_clob = t.add(clobber, k2);
    let both = t.add(a_res, a_clob);
    t.ret_val(both)
}

#[test]
fn res_pins_the_result_output_excluding_clobbers() {
    let f = call_other_result_and_clobber_feed_adds();
    let m = Matcher::new(&f);
    // Loose: both the result-fed and clobber-fed adds match (2 value outputs).
    assert_eq!(
        m.find_all(&add(any(), call_other().name("getval")).into_pattern())
            .unwrap()
            .len(),
        2,
    );
    // `.res()` pins the declared result (raw slot 2) → only the result-fed add.
    assert_eq!(
        m.find_all(&add(any(), call_other().name("getval").res()).into_pattern())
            .unwrap()
            .len(),
        1,
    );
}
