//! A pattern is untrusted input: a spelling the engine cannot honour is
//! refused through the query's `Result`, never by panicking and never by
//! silently matching nothing.

use strider_ir::IntBinaryOp;
use strider_ir::node::NodeKind;
use strider_ir_test_utils::Tb;
use strider_pattern::matcher::{KindSpec, MatcherBuilder};
use strider_pattern::template::TemplateBuilder;
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

/// The template side of the same wiring.
#[test]
fn a_cyclic_template_wiring_is_refused_not_panicked() {
    let mut b = TemplateBuilder::new();
    let n = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
    let out = b.value_output(n, 0);
    b.input(n, 0, out);
    let err = b.finish().root().unwrap_err().to_string();
    assert!(err.contains("cycle"), "{err}");
}

/// The first refusal is the one reported, and it is preferred over the cycle
/// verdict on whatever the refused lowering left staged. The template side is
/// pinned in `template::builder`.
#[test]
fn the_first_refusal_wins_over_later_ones_and_a_cycle() {
    let mut b = MatcherBuilder::new();
    let n = b.node(KindSpec::Any);
    let out = b.value_output(n, 0);
    b.input(n, 0, out);
    b.reject("first".into());
    b.reject("second".into());
    assert_eq!(error_of(&b.finish()), "first");
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

/// An operand slot takes what its operand nests through. A memory token, a
/// control edge or a node with no output in a value slot, and a memory token
/// or a node with no output in a control slot, can never match there.
#[test]
fn an_operand_with_nothing_to_nest_through_is_refused() {
    use strider_pattern::{call, entry, load, mem_phi, store, unreachable};
    let x = strider_pattern::Capture::new();
    for (pat, why) in [
        (int_add(store(), var(x)).into_pattern(), "memory token"),
        (
            int_add(var(x), mem_phi().capture(x)).into_pattern(),
            "memory token",
        ),
        (int_add(entry(), var(x)).into_pattern(), "control edge"),
        (int_add(ret(), var(x)).into_pattern(), "no output"),
        (load().addr(if_else()).build(), "no output"),
        (
            int_add(store().when_match(|_m, _t, _b| true), var(x)).into_pattern(),
            "memory token",
        ),
        (ret().ctrl(ret()).build(), "no output"),
        (call().ctrl(store()).build(), "memory token"),
        (ret().ctrl(unreachable()).build(), "no output"),
        (
            strider_pattern::float_le(store(), var(x)).into_pattern(),
            "memory token",
        ),
        (
            strider_pattern::float_is_nan(mem_phi()).into_pattern(),
            "memory token",
        ),
    ] {
        let err = error_of(&pat);
        assert!(err.contains(why), "{err}");
    }
}

/// The slots that take any edge, the alternation arms, and the control
/// slot's control producers stay admitted.
#[test]
fn an_operand_that_can_nest_is_admitted() {
    use strider_pattern::{call, entry, load, region, store};
    let function = add_5_3();
    let matcher = Matcher::new(&function);
    for pat in [
        load().input(0, store()).build(),
        call().any_input(store()).build(),
        region().input(0, entry()).build(),
        ret().ctrl(if_else()).build(),
        ret().ctrl(region()).build(),
        ret().ctrl(call()).build(),
        int_add(one_of![store(), anything()], anything()).into_pattern(),
        one_of![ret(), if_else()].into_pattern(),
        strider_pattern::phi().phi_token(region()).build(),
        strider_pattern::mem_phi().phi_token(region()).build(),
        strider_pattern::call_other().arg(0, entry()).build(),
        strider_pattern::call_other().arg(1, store()).build(),
    ] {
        assert!(matcher.find_all(&pat).is_ok());
    }
}

/// The hidden `expr()` reaches every pattern, so a part used on a side it is
/// not valid on is refused through the root, never by panicking.
#[test]
fn a_part_on_the_wrong_side_is_refused() {
    use strider_pattern::{TemplatePat, capture_typed, int_const_with_fn, template};
    let a = strider_pattern::Capture::new();
    let wildcard = template::int_add(anything().expr(), var(a));
    assert!(TemplatePat::into_template(wildcard).root().is_err());
    let built = TemplatePat::expr(int_const_with_fn(|_| Ok(1u8.into())));
    assert!(error_of(&int_add(built, anything()).into_pattern()).contains("build-only"));
    let typed = TemplatePat::expr(capture_typed(a, var(a)));
    assert!(error_of(&int_add(typed, anything()).into_pattern()).contains("build-only"));
    let node = template::int_add(strider_pattern::call().expr(), var(a));
    assert!(TemplatePat::into_template(node).root().is_err());
}

/// Implements both traits by hand, so a factory of either side holds it
/// lowered through that side's methods only.
struct Both(strider_pattern::Capture);

impl MatchPat for Both {
    fn compile(self, b: &mut MatcherBuilder) -> strider_pattern::matcher::PatValueRef {
        MatchPat::compile(var(self.0), b)
    }
}

impl strider_pattern::TemplatePat for Both {
    fn compile(self, b: &mut TemplateBuilder) -> strider_pattern::template::TmplValueRef {
        strider_pattern::TemplatePat::compile(var(self.0), b)
    }
}

#[test]
fn a_hand_written_pattern_on_the_other_side_is_refused() {
    use strider_pattern::{TemplatePat, template};
    let c = strider_pattern::Capture::new();
    let built = TemplatePat::into_template(int_add(Both(c), Both(c)));
    assert!(built.root().is_err());
    let matched = template::int_add(Both(c), Both(c)).into_pattern();
    assert!(error_of(&matched).contains("other side"));
    // Each side's own factory still lowers it.
    assert!(
        TemplatePat::into_template(template::int_add(Both(c), Both(c)))
            .root()
            .is_ok()
    );
    assert!(int_add(Both(c), Both(c)).into_pattern().root().is_ok());
}

/// On the build side only a `var()`'s capture names the binding it reuses: a
/// captured constant, predicate or kind would vanish into a binding reuse.
#[test]
fn a_captured_non_var_leaf_is_refused_on_the_build_side() {
    use strider_pattern::{
        Capture, TemplatePat, any_int_const, float_const, initial_var, template,
    };
    let (a, c) = (Capture::new(), Capture::new());
    let built = |leaf: strider_pattern::Expr| {
        TemplatePat::into_template(template::int_add(leaf, var(a))).root()
    };
    assert!(built(any_int_const().capture(c).expr()).is_err());
    assert!(built(float_const(5u64).capture(c).expr()).is_err());
    assert!(built(initial_var().capture(c).expr()).is_err());
    assert!(built(MatchPat::expr(var(c))).is_ok());
    assert!(built(anything().capture(c).expr()).is_ok());
}
