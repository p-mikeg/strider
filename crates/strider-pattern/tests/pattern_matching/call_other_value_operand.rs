//! `CallOther` / `Call` nested as a value operand, e.g.
//! `add(x, call_other().name("f"))`. Loose by default: any value output of the
//! matching call satisfies the operand. `.out(i)` pins a specific one.

use strider_ir::Function;
use strider_ir_test_utils::{Tb, reg_vn};
use strider_pattern::{MatchPat, Matcher, add, any, call_other, int_const};

/// `add(getval(), 5)`, returned so the add stays reachable.
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
    assert_eq!(
        m.find_all(&add(any(), call_other().name("getval")).into_pattern())
            .unwrap()
            .len(),
        1,
    );
    assert_eq!(
        m.find_all(&add(any(), call_other().name("nope")).into_pattern())
            .unwrap()
            .len(),
        0,
    );
    // The other operand stays matchable alongside the call.
    assert_eq!(
        m.find_all(&add(int_const(5u128), call_other().name("getval")).into_pattern())
            .unwrap()
            .len(),
        1,
    );
}

/// `getval()` producing a result register plus a clobber register, each
/// feeding its own `add`. The adds are summed into the return value to keep
/// both reachable.
fn call_other_result_and_clobber_feed_adds() -> Function {
    let res = reg_vn(0x10, 8);
    let clob = reg_vn(0x20, 8);
    let mut t = Tb::with_vars(&[res, clob]);
    let result = t
        .call_other("getval", 42, &[], Some(res), &[], &[clob])
        .expect("getval has a result output");
    let clobber = t.read_var(&clob);
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
    // Loose match: two value outputs, so both adds hit.
    assert_eq!(
        m.find_all(&add(any(), call_other().name("getval")).into_pattern())
            .unwrap()
            .len(),
        2,
    );
    // .res() pins the declared result at raw slot 2, leaving only one add.
    assert_eq!(
        m.find_all(&add(any(), call_other().name("getval").res()).into_pattern())
            .unwrap()
            .len(),
        1,
    );
}
