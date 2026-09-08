//! A pattern is untrusted input: a spelling the engine cannot honour is
//! refused through the query's `Result`, never by panicking and never by
//! silently matching nothing.

use strider_ir::IntBinaryOp;
use strider_ir_test_utils::Tb;
use strider_pattern::matcher::{KindSpec, MatcherBuilder};
use strider_pattern::{
    CaptureExt, MatchPat, Matcher, Pattern, anything, if_else, int_add, one_of, ret, var,
};

fn add_5_3() -> strider_ir::Function {
    let mut t = Tb::empty();
    let a = t.u64(5);
    let b = t.u64(3);
    let sum = t.add(a, b);
    t.ret_val(sum)
}

fn multi_sink_pattern() -> Pattern {
    let mut b = MatcherBuilder::new();
    let _a = b.leaf(KindSpec::Any);
    let _b = b.leaf(KindSpec::Any);
    b.finish()
}

fn error_of(pat: &Pattern) -> String {
    let function = add_5_3();
    match Matcher::new(&function).find_all(pat) {
        Ok(hits) => panic!("pattern must be refused, got {} matches", hits.len()),
        Err(e) => e.to_string(),
    }
}

/// `when_match` guards the matched output's `ValueType`. A control-rooted
/// builder has none, so the guard could never run; a `|_| true` guard silently
/// matching nothing is worse than an error.
#[test]
fn a_typed_guard_on_a_control_rooted_builder_is_refused() {
    let pat = if_else().when_match(|_m, _ty, _b| true).into_pattern();
    assert!(error_of(&pat).contains("with_root_post_match"));

    let pat = ret().build();
    // The same builder without the guard still matches.
    let function = add_5_3();
    assert_eq!(Matcher::new(&function).find_all(&pat).unwrap().len(), 1);

    let pat = strider_pattern::store()
        .when_match(|_m, _ty, _b| true)
        .into_pattern();
    assert!(error_of(&pat).contains("with_root_post_match"));
}

/// A typed guard on a value root is untouched.
#[test]
fn a_typed_guard_on_a_value_root_still_runs() {
    let function = add_5_3();
    let pat = int_add(anything(), anything())
        .when_match(|_m, _ty, _b| true)
        .into_pattern();
    assert_eq!(Matcher::new(&function).find_all(&pat).unwrap().len(), 1);
}

/// A multi-sink branch pattern is not matchable. Everywhere else that is an
/// `Err`; `with_true` / `with_false` used to panic on it.
#[test]
fn a_multi_sink_branch_pattern_is_refused_not_panicked_on() {
    let pat = if_else().with_true(multi_sink_pattern()).build();
    assert!(error_of(&pat).contains("branch pattern"));

    let pat = if_else().with_false(multi_sink_pattern()).build();
    assert!(error_of(&pat).contains("branch pattern"));
}

/// `.ordered()` suppresses commutative operand reordering. An alternation's
/// inputs are alternatives, not operands, so the engine never reads the flag.
#[test]
fn ordered_on_an_alternation_is_refused() {
    let x = strider_pattern::Capture::new();
    let pat = one_of![int_add(var(x), anything()), anything()]
        .ordered()
        .into_pattern();
    assert!(error_of(&pat).contains("ordered"));
}

/// `.ordered()` on an operand node is unaffected.
#[test]
fn ordered_on_a_commutative_node_still_pins_the_operand_order() {
    let function = add_5_3();
    let x = strider_pattern::Capture::new();
    let pat = int_add(var(x), anything()).ordered().into_pattern();
    let hits = Matcher::new(&function).find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1, "one ordering, not two");
    let _ = IntBinaryOp::Add;
}
