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
