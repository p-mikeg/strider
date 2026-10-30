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

/// `k` adds over ONE shared wildcard leaf, each add feeding both slots of the
/// next: `k + 1` nodes, `2^(k+1) - 1` walks.
fn doubling_chain(k: usize) -> Pattern {
    let mut b = MatcherBuilder::new();
    let mut o = b.leaf(KindSpec::Any);
    b.set_output_any(o);
    for _ in 0..k {
        o = b.binary(IntBinaryOp::Add, o, o);
    }
    b.finish()
}

/// A vertex feeding both slots is walked once per slot, so the stack grows with
/// the unfolded walk count and a 9-node pattern already overflows the 2 MiB a
/// spawned thread gets. The cap charges the unfolding and refuses on the normal
/// channel, which is what the contract on `MAX_PATTERN_NODES` promises.
#[test]
fn a_shared_vertex_pattern_over_the_unfolded_cap_is_refused() {
    assert!(
        doubling_chain(6).root().is_ok(),
        "127 walks is under the cap"
    );
    for k in [8usize, 12, 16] {
        std::thread::spawn(move || {
            assert!(
                error_of(&doubling_chain(k)).contains("nodes"),
                "{} nodes sharing one leaf must be refused",
                k + 1
            );
        })
        .join()
        .unwrap();
    }
}

/// A chain of `k` adds, each over a fresh leaf, offered as `arms` alternatives
/// of one alternation. Every arm is the SAME vertex, so the alternation's own
/// cost is one arm's, not the sum: `2k + 1` nodes tree-unfolded.
fn shared_alternation(arms: usize, k: usize) -> Pattern {
    let mut b = MatcherBuilder::new();
    let mut o = b.leaf(KindSpec::Any);
    b.set_output_any(o);
    for _ in 0..k {
        let leaf = b.leaf(KindSpec::Any);
        o = b.binary(IntBinaryOp::Add, o, leaf);
    }
    let alts = vec![o; arms];
    let alt = b.one_of(&alts);
    b.set_output_any(alt);
    b.finish()
}

/// `try_alternation` runs an alternation's arms one at a time and unwinds
/// between them, so the arms do not stack: the cap must charge the deepest arm,
/// not their sum, or it refuses a pattern the engine runs.
#[test]
fn an_alternations_arms_are_charged_at_their_deepest_not_their_sum() {
    // Accepted AND run on a 2 MiB spawned-thread stack, which is what the cap
    // exists to bound.
    for arms in [2usize, 4, 8] {
        std::thread::spawn(move || {
            let pat = shared_alternation(arms, 40);
            let function = add_5_3();
            assert!(
                Matcher::new(&function).find_all(&pat).is_ok(),
                "{arms} arms of 81 unfolded nodes each run one at a time",
            );
        })
        .join()
        .unwrap();
    }
    // The deepest arm alone still has to fit.
    assert!(error_of(&shared_alternation(2, 300)).contains("nodes"));
}

/// `levels` alternations, each the one arm of the next, over a wildcard leaf:
/// one node per level plus the leaf, and every match recurses through each.
fn nested_alternations(levels: usize) -> Pattern {
    let mut b = MatcherBuilder::new();
    let mut o = b.leaf(KindSpec::Any);
    b.set_output_any(o);
    for _ in 0..levels {
        o = b.one_of(&[o]);
    }
    b.finish()
}

/// 255 nested alternations and their leaf are 256 nodes, exactly the budget,
/// and match on the 2 MiB stack a spawned thread gets by default, as often as
/// the leaf alone.
#[test]
fn nested_alternations_at_the_node_cap_match() {
    assert!(error_of(&nested_alternations(256)).contains("nodes"));
    std::thread::spawn(|| {
        let function = add_5_3();
        let matcher = Matcher::new(&function);
        let bare = matcher.find_all(&nested_alternations(0)).unwrap().len();
        assert!(bare > 0);
        let pat = nested_alternations(255);
        assert_eq!(matcher.find_all(&pat).unwrap().len(), bare);
    })
    .join()
    .unwrap();
}
