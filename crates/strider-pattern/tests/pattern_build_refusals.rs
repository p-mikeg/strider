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

/// A `ctrl()` slot retypes its operand to `Control` only after compiling it,
/// so the guard's build-time refusal has to re-run there. Without it the
/// query returns nothing and reports no error.
#[test]
fn a_typed_guard_in_a_control_slot_is_refused() {
    let pat = strider_pattern::call()
        .ctrl(anything().when_match(|_m, _ty, _b| true))
        .build();
    assert!(error_of(&pat).contains("with_root_post_match"));

    let pat = strider_pattern::call()
        .ctrl(one_of![anything().when_match(|_m, _ty, _b| true)])
        .build();
    assert!(
        error_of(&pat).contains("with_root_post_match"),
        "an alternation arm is retyped with its alternation",
    );
}

/// The same slot without the guard still matches, so the refusal is about the
/// guard rather than the `ctrl` operand.
#[test]
fn an_unguarded_control_slot_operand_still_matches() {
    let mut t = Tb::empty();
    t.call_at(0x1000);
    let function = t.ret_nothing();
    let pat = strider_pattern::call().ctrl(anything()).build();
    assert_eq!(Matcher::new(&function).find_all(&pat).unwrap().len(), 1);
}

/// A chain of `adds` add nodes over `adds + 1` wildcard leaves.
fn add_chain(adds: usize) -> Pattern {
    let mut b = MatcherBuilder::new();
    let mut o = b.leaf(KindSpec::Any);
    b.set_output_any(o);
    for _ in 0..adds {
        let leaf = b.leaf(KindSpec::Any);
        b.set_output_any(leaf);
        o = b.binary(IntBinaryOp::Add, o, leaf);
    }
    b.finish()
}

/// The engine recurses once per pattern NODE, so an unbounded pattern aborts
/// the process instead of erroring. The cap is a refusal on the normal channel.
#[test]
fn a_pattern_at_the_node_cap_still_builds() {
    let pat = add_chain(127);
    assert!(pat.root().is_ok());
    let function = add_5_3();
    assert!(Matcher::new(&function).find_all(&pat).is_ok());
}

#[test]
fn a_pattern_over_the_node_cap_is_refused() {
    assert!(error_of(&add_chain(128)).contains("nodes"));
}

/// Nested alternations recurse at COMPILE time, before a node count exists.
#[test]
fn a_deeply_nested_alternation_is_refused() {
    fn nest(depth: u32) -> strider_pattern::OneOf {
        let inner: strider_pattern::BoxedAlt = if depth == 0 {
            strider_pattern::boxed_alt(anything())
        } else {
            strider_pattern::boxed_alt(nest(depth - 1))
        };
        strider_pattern::OneOf::new(vec![inner])
    }
    assert!(!error_of(&nest(4000).into_pattern()).is_empty());
}

/// A `call` per level, each nested in the previous one's arg slot.
fn deep_call_chain(depth: usize) -> strider_pattern::CallPat {
    let mut p = strider_pattern::call();
    for _ in 0..depth {
        p = strider_pattern::call().arg(0, p);
    }
    p
}

/// A `one_of` per level, each the sole arm of the previous one.
fn deep_alternation(depth: usize) -> strider_pattern::OneOf {
    let mut p = one_of![anything()];
    for _ in 0..depth {
        p = one_of![p];
    }
    p
}

/// Every operand slot is one lowering frame, so an unbounded chain aborted the
/// process before a node count existed to refuse it by.
#[test]
fn a_deep_operand_chain_is_refused_not_aborted() {
    for depth in [6_000, 12_000, 200_000] {
        assert!(
            error_of(&deep_call_chain(depth).into_pattern()).contains("nests"),
            "depth {depth} must be refused for nesting",
        );
        assert!(
            error_of(&deep_alternation(depth).into_pattern()).contains("nests"),
            "depth {depth} must be refused for nesting",
        );
    }
}

/// The tower under an uncompiled builder is a chain of boxed closures, one link
/// per level, so dropping it unwinds a frame per level unless the drop defers.
#[test]
fn an_uncompiled_deep_tower_drops_without_recursing() {
    drop(deep_call_chain(200_000));
    drop(deep_alternation(200_000));
}

/// The nesting cap refuses nothing the node cap accepts: 255 nested calls are
/// 256 nodes, exactly the node budget.
#[test]
fn a_chain_at_the_nesting_cap_still_builds() {
    assert!(deep_call_chain(255).into_pattern().root().is_ok());
}

/// A node wired to its own output is a cycle; `MatcherBuilder` takes untrusted
/// wiring, so the seal refuses it rather than panicking.
#[test]
fn a_cyclic_builder_wiring_is_refused_not_panicked() {
    let mut b = MatcherBuilder::new();
    let n = b.node(KindSpec::Any);
    let out = b.value_output(n, 0);
    b.input(n, 0, out);
    assert!(error_of(&b.finish()).contains("cycle"));
}

/// `n` Ifs, each true edge feeding the next If's control input directly, every
/// false edge running to one exit.
fn if_chain(n: usize) -> strider_ir::Function {
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{EditFunction, IRBuilderExt, IRViewer, IntCmpOp};

    let mut b = strider_ir_test_utils::RegisterSet::new()
        .build_fn()
        .unwrap();
    let regions: Vec<_> = (0..=n).map(|_| b.create_region_all().unwrap()).collect();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(regions[0]).unwrap();
    for (i, pair) in regions.windows(2).enumerate() {
        b.set_region(pair[0]);
        let c = b.build_int_const(i as u64, ValueType::I8).unwrap();
        let z = b.build_int_const(0u64, ValueType::I8).unwrap();
        let cond = b
            .build_int_cmp_operation(c, z, IntCmpOp::Equal, ValueType::I8)
            .unwrap();
        b.build_if(cond, pair[1], exit).unwrap();
    }
    b.set_region(regions[n]);
    b.build_branch(exit).unwrap();
    b.set_region(exit);
    b.build_return(None, &[]).unwrap();
    let mut f = b.build().unwrap();

    let singles: Vec<_> = f
        .graph()
        .all_node_ids()
        .filter(|&r| matches!(f.node_kind(r), NodeKind::Region))
        .filter(|&r| {
            let inputs = f.node_inputs(r);
            inputs.len() == 1 && matches!(f.node_kind(f.producer(inputs[0])), NodeKind::If)
        })
        .collect();
    let mut e = EditFunction::new(&mut f);
    for r in singles {
        let token = e.function().node_outputs(r)[1];
        let phis: Vec<_> = e
            .function()
            .graph()
            .value_uses(token)
            .map(|(n, _)| n)
            .collect();
        for phi in phis {
            let out = e.function().node_outputs(phi)[0];
            let only = e.function().node_inputs(phi)[1];
            e.replace_all_uses(out, only).unwrap();
        }
        let pred = e.function().node_inputs(r)[0];
        let ctrl = e.function().node_outputs(r)[0];
        e.replace_all_uses(ctrl, pred).unwrap();
        e.kill_node(r);
    }
    e.clean();
    strider_ir::validate::validate(&f).expect("collapsed chain is valid IR");
    f
}

/// `depth` Ifs, each the true branch of the one above.
fn nested_if_branches(depth: usize) -> Pattern {
    let mut p = if_else().build();
    for _ in 0..depth {
        p = if_else().with_true(p).build();
    }
    p
}

/// Every branch is its own sealed pattern, but the matcher recurses through
/// all of them on one stack, so the node cap counts the nested ones too.
#[test]
fn nested_if_branches_over_the_node_cap_are_refused() {
    assert!(error_of(&nested_if_branches(256)).contains("nodes"));
    assert!(error_of(&nested_if_branches(3_000)).contains("nodes"));
}

/// 255 nested branches are 256 If nodes, exactly the budget, and match on the
/// 2 MiB stack a spawned thread gets by default.
#[test]
fn nested_if_branches_at_the_node_cap_match() {
    std::thread::spawn(|| {
        let function = if_chain(256);
        let pat = nested_if_branches(255);
        assert_eq!(Matcher::new(&function).find_all(&pat).unwrap().len(), 1);
    })
    .join()
    .unwrap();
}
