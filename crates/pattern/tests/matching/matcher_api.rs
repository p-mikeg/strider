//! `Matcher` API surface: `find_all`, `match_at`, `function_arg*`, and the
//! kind-prefilter early-out.

use ir::IntBinaryOp;
use ir::node::{NodeId, NodeKind};
use pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Matcher::new / empty graphs ──────────────────────────────────────────────

#[test]
fn new_on_empty_graph_does_not_panic() {
    // Empty function: just entry → return-nothing.
    let g = Tb::empty().ret_nothing();
    let _ = Matcher::new(&g);
}

#[test]
fn find_all_on_empty_graph_returns_empty_for_specific_kind() {
    let g = Tb::empty().ret_nothing();
    a::none(&g, load());
    a::none(&g, call());
    a::none(&g, add(any(), any()));
}

// ── Kind-prefilter correctness ───────────────────────────────────────────────

#[test]
fn kind_prefilter_skips_incompatible_nodes() {
    // add(5, 3) has only IntConst + IntBinaryOp + Return + Entry.  load()
    // pattern is kind-filtered to Load, so it must return 0 matches without
    // attempting any structural work.
    let g = shapes::add_consts(5, 3);
    a::none(&g, load());
    a::none(&g, store());
    a::none(&g, call());
    a::none(&g, if_node());
    a::none(&g, phi());
}

// ── match_at ─────────────────────────────────────────────────────────────────

#[test]
fn match_at_hits_correct_node() {
    let g = shapes::add_consts(5, 3);
    // Find the Add node directly.
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .expect("add node exists");

    let m = Matcher::new(&g)
        .match_at(add_node, &add(int_const(5), int_const(3)).into())
        .expect("match_at should succeed on the Add node");
    assert_eq!(m.root(), add_node);
}

#[test]
fn match_at_on_wrong_kind_returns_none() {
    let g = shapes::add_consts(5, 3);
    // Apply `load()` at the Add node — kind mismatch → None, no panic.
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();

    let result = Matcher::new(&g).match_at(add_node, &load().into());
    assert!(result.is_none());
}

#[test]
fn match_at_is_scoped_to_that_node_only() {
    // Graph has two IntConsts (5 and 3) and one Add.
    let g = shapes::add_consts(5, 3);
    let add_node = g
        .preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .unwrap();

    // Ask `int_const(5)` at the Add node: the pattern is for IntConst, Add
    // has a different kind → None.
    let result = Matcher::new(&g).match_at(add_node, &int_const(5));
    assert!(result.is_none());
}

// ── find_all deterministic ordering ──────────────────────────────────────────

#[test]
fn find_all_is_deterministic() {
    let g = shapes::add_nested_3(1, 2, 3);
    let matcher = Matcher::new(&g);
    let pat: Pat = any_int_const(IntVar::new());

    let a1: Vec<_> = matcher.find_all(&pat).into_iter().map(|m| m.root()).collect();
    let a2: Vec<_> = matcher.find_all(&pat).into_iter().map(|m| m.root()).collect();

    assert_eq!(a1, a2);
}

// ── function_arg*: empty graph ───────────────────────────────────────────────

#[test]
fn function_arg_apis_on_graph_without_args() {
    let g = shapes::add_consts(5, 3);
    let matcher = Matcher::new(&g);

    assert!(matcher.function_arg(0).is_none());
    assert_eq!(matcher.function_arg_count(), 0);
    assert_eq!(matcher.function_arg_len(), 0);
    assert_eq!(matcher.function_args().count(), 0);
}

// ── Multi-match iteration & distinct bindings ────────────────────────────────

/// Graph with three distinct Add nodes at different operand values; the
/// `add(any_int_const, any_int_const)` pattern should find all three and
/// each `Match` should carry its own captures.
fn graph_three_adds() -> ir::BuiltFunctionGraph {
    let mut t = Tb::empty();
    let a = t.u64(1);
    let b = t.u64(2);
    let c = t.u64(3);
    let d = t.u64(4);
    let e = t.u64(5);
    let f = t.u64(6);
    let s1 = t.add(a, b); // add(1, 2)
    let s2 = t.add(c, d); // add(3, 4)
    let s3 = t.add(e, f); // add(5, 6)
    // Thread all three into the Return via an outer Add so none are dead.
    let s12 = t.add(s1, s2);
    let final_ = t.add(s12, s3);
    t.ret_val(final_)
}

#[test]
fn find_all_returns_distinct_matches_with_distinct_roots() {
    let g = graph_three_adds();
    let lhs = IntVar::new();
    let rhs = IntVar::new();
    let hits = Matcher::new(&g).find_all(&add(any_int_const(lhs), any_int_const(rhs)).into());
    // Three leaf Adds + two outer Adds (the outer ones have non-const
    // operands, so they DON'T match `any_int_const × any_int_const`).
    assert_eq!(hits.len(), 3);

    // All three `root` NodeIds are distinct.
    let roots: std::collections::HashSet<NodeId> = hits.iter().map(|m| m.root()).collect();
    assert_eq!(roots.len(), 3);
}

#[test]
fn each_match_has_its_own_bindings() {
    let g = graph_three_adds();
    let lhs = IntVar::new();
    let rhs = IntVar::new();
    let hits = Matcher::new(&g).find_all(&add(any_int_const(lhs), any_int_const(rhs)).into());
    assert_eq!(hits.len(), 3);

    // Gather the (lhs, rhs) captures across all matches as an order-
    // independent set.  Exactly the three operand pairs must appear.
    let mut got: Vec<(u128, u128)> = hits
        .iter()
        .map(|m| (m.get_int_var(lhs).unwrap(), m.get_int_var(rhs).unwrap()))
        .map(|(l, r)| if l < r { (l, r) } else { (r, l) }) // commutative retry can swap
        .collect();
    got.sort();
    assert_eq!(got, vec![(1, 2), (3, 4), (5, 6)]);
}

// ── Match::bindings_clone ────────────────────────────────────────────────────

#[test]
fn bindings_clone_outlives_match() {
    let g = shapes::add_consts(5, 3);
    let v = IntVar::new();

    // Clone the bindings, then drop the Match.  The snapshot still resolves.
    let bindings = {
        let matcher = Matcher::new(&g);
        let m = matcher
            .match_at(
                g.preorder()
                    .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntConst(5)))
                    .unwrap(),
                &any_int_const(v),
            )
            .expect("match");
        m.bindings_clone()
    };

    assert_eq!(bindings.get_int(v), Some(5));
}

// ── Match::get_vn ────────────────────────────────────────────────────────────

#[test]
fn get_vn_on_initial_var_returns_varnode() {
    let (g, reg) = shapes::single_initial_var();
    let v = Capture::new();
    let m = a::unique(&g, initial_var().capture(v));
    assert_eq!(m.get_vn(v, &g), Some(reg));
}

#[test]
fn get_vn_on_non_mapped_producer_returns_none() {
    let g = shapes::add_consts(5, 3);
    let v = Capture::new();
    // Capture the Add itself — `get_vn` only has a meaning for InitialVar
    // and Call ret-output slots, so this must return None.
    let m = a::unique(&g, add(int_const(5), int_const(3)).capture(v));
    assert_eq!(m.get_vn(v, &g), None);
}

#[test]
fn get_vn_on_call_ret_output_returns_ret_reg() {
    // Single-ret-reg ABI stub; Call output slot 2 represents the post-call
    // value of `ret`.
    let ret = super::support::reg_vn(0, 8);
    let mut t = Tb::raw(vec![ret], &[], &[], &[ret], None, 0);
    t.call_at(0xCAFE);
    let g = t.ret_regs(&[ret]);

    let v = Capture::new();
    let m = a::unique(&g, call().at(0xCAFE).ret_output(0, var(v)));
    assert_eq!(m.get_vn(v, &g), Some(ret));
}

#[test]
fn get_vn_on_unbound_var_returns_none() {
    let g = shapes::add_consts(5, 3);
    let m = a::first(&g, int_const(5));
    let never_bound = Capture::new();
    assert_eq!(m.get_vn(never_bound, &g), None);
}

// ── MatcherOptions: ignore_casts / ignore_control_states flags ──────────────
//
// The flags default to off (strict exact-walk semantics).  Phases 2/3/4
// implement the actual walk-through; Phase 1 only adds the API surface
// and verifies the existing matcher behavior is unchanged when both flags
// stay at their defaults.  These tests pin those contracts so a future
// refactor of the option type doesn't silently change defaults.

#[test]
fn matcher_default_options_are_both_off() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g);
    let opts = m.options_for_test();
    assert!(
        opts.ignore_cast_mask.is_empty(),
        "ignore_cast_mask must default to empty"
    );
    assert!(
        !opts.ignore_control_states,
        "ignore_control_states must default to false"
    );
}

#[test]
fn ignore_casts_chains_and_flips_flag() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g).ignore_casts();
    let opts = m.options_for_test();
    assert_eq!(
        opts.ignore_cast_mask,
        CastMask::all(),
        "ignore_casts() must set the mask to CastMask::all()"
    );
    assert!(
        !opts.ignore_control_states,
        "ignore_casts() must not touch ignore_control_states"
    );
}

#[test]
fn ignore_control_states_chains_and_flips_flag() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g).ignore_control_states();
    let opts = m.options_for_test();
    assert!(
        opts.ignore_control_states,
        "ignore_control_states() must enable the flag"
    );
    assert!(
        opts.ignore_cast_mask.is_empty(),
        "ignore_control_states() must not touch ignore_cast_mask"
    );
}

#[test]
fn both_flags_chain_independently() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g).ignore_casts().ignore_control_states();
    let opts = m.options_for_test();
    assert_eq!(opts.ignore_cast_mask, CastMask::all());
    assert!(opts.ignore_control_states);
}

/// Regression: with both flags off, existing pattern queries return the
/// same matches as before.  If a future contributor flips a default,
/// this test catches the silent change.
#[test]
fn existing_pattern_unchanged_with_default_options() {
    // `add(5, 3)` graph has exactly one Add and the matcher must find it
    // — same as the existing kind-prefilter test, but explicitly via a
    // `Matcher::new` (no flags).
    let g = shapes::add_consts(5, 3);
    let pat: Pat = add(int_const(5), int_const(3)).into();
    let hits = Matcher::new(&g).find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "Matcher::new (default options) must find the Add — flags off must \
         preserve existing behavior"
    );
}

// ── ignore_casts walk-through ────────────────────────────────────────────────
//
// Build a small graph that contains an `Add(Extend(Mul), c)` shape — the
// canonical x86/x64 width-cast problem.  Without `ignore_casts` the strict
// matcher can't see the Mul through the Extend; with the flag set, it does.

/// Returns a graph whose return value is `Add(ZeroExt(Mul(2,3)), 4)` at U64,
/// where the Mul is at U32.  Mirrors x64's IMUL register-merge chain in
/// miniature.
fn graph_add_zext_mul() -> ir::BuiltFunctionGraph {
    use ir::node::NodeOutputType;
    let mut t = Tb::empty();
    let two = t.u32(2);
    let three = t.u32(3);
    let mul = t.int_bin_at(two, three, ir::IntBinaryOp::Mul, NodeOutputType::U32);
    let widened = t.zext_to(mul, NodeOutputType::U64);
    let four = t.u64(4);
    let total = t.add(widened, four);
    t.ret_val(total)
}

/// Strict pattern `add(mul(_,_), _)` against `Add(ZeroExt(Mul), c)` must
/// fail without `ignore_casts` — pinning the pre-fix behavior so a later
/// contributor doesn't accidentally add transparent walking by default.
#[test]
fn add_mul_pattern_does_not_match_through_extend_by_default() {
    let g = graph_add_zext_mul();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::new(&g).find_all(&pat);
    assert!(
        hits.is_empty(),
        "default matcher must NOT walk through Extend; got {} hits",
        hits.len()
    );
}

/// `add(mul(_,_), _)` finds the Mul through an intervening ZeroExtend
/// when `ignore_casts` is set.
#[test]
fn add_mul_pattern_matches_through_extend_with_ignore_casts() {
    let g = graph_add_zext_mul();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::new(&g).ignore_casts().find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "ignore_casts must let add(mul,_,_) walk through Extend to find Mul; \
         got {} hits",
        hits.len()
    );
}

/// Walk-through must chain through multiple casts (Mul → Trunc → Extend).
/// Tests that `match_one` recurses into the cast's input and the recursive
/// match itself benefits from the same flag.
#[test]
fn add_mul_pattern_matches_through_chained_casts() {
    use ir::node::NodeOutputType;
    let g = {
        let mut t = Tb::empty();
        let two = t.u64(2);
        let three = t.u64(3);
        let mul = t.mul(two, three);
        let truncated = t.trunc_to(mul, NodeOutputType::U32);
        let widened = t.zext_to(truncated, NodeOutputType::U64);
        let four = t.u64(4);
        let total = t.add(widened, four);
        t.ret_val(total)
    };
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::new(&g).ignore_casts().find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "ignore_casts must walk through chained Trunc+Extend to reach Mul"
    );
}

/// Strict patterns that explicitly ask for a cast (e.g. `truncate(x)`)
/// continue to match the literal cast node when `ignore_casts` is set —
/// the direct-match-first ordering preserves it.  Without this guarantee,
/// `truncate(x)` would silently walk through to `x`.
#[test]
fn truncate_pattern_still_matches_truncate_with_ignore_casts() {
    use ir::node::{NodeKind, NodeOutputType};
    let g = {
        let mut t = Tb::empty();
        // Use a non-const expression so `truncate_if_needed` actually
        // emits a Truncate node (it short-circuits on IntConst inputs).
        let a = t.u64(0xDEAD_BEEF);
        let b = t.u64(0x1234_5678);
        let or = t.bor(a, b);
        let truncated = t.trunc_to(or, NodeOutputType::U32);
        t.ret_val(truncated)
    };
    // `truncate(any())` must match the Truncate node directly, NOT walk
    // through to the IntConst behind it.
    let m = Matcher::new(&g).ignore_casts();
    let hits = m.find_all(&truncate(any()));
    assert_eq!(hits.len(), 1, "truncate(any) must match the Truncate node");
    assert!(matches!(
        g.graph.node_kind(hits[0].root()),
        NodeKind::Truncate
    ));
}

/// Commutative retry interacts cleanly with walk-through: if the LHS
/// fails to match directly and as-cast, the matcher swaps and retries
/// the RHS as the cast-bearing operand.
#[test]
fn commutative_add_finds_mul_in_either_operand_through_extend() {
    use ir::node::NodeOutputType;
    // `Add(arg, ZeroExt(Mul))` — Mul is on the RHS, behind an Extend.
    let g = {
        let mut t = Tb::empty();
        let two = t.u32(2);
        let three = t.u32(3);
        let mul = t.int_bin_at(two, three, ir::IntBinaryOp::Mul, NodeOutputType::U32);
        let widened = t.zext_to(mul, NodeOutputType::U64);
        let four = t.u64(4);
        // Note the operand order: arg first, mul-via-extend second.
        let total = t.add(four, widened);
        t.ret_val(total)
    };
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::new(&g).ignore_casts().find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "commutative add must find Mul-via-Extend on either operand"
    );
}

// ── ignore_control_states walk-through ──────────────────────────────────────
//
// Build a graph where Return ← ControlState ← Call.  Without the flag,
// `ret().preceded_by(call())` matches against ControlState (the Return's
// direct ctrl input is the ControlState's Control output) and fails.
// With `ignore_control_states`, the walk-through tries each of the
// ControlState's control inputs — finding the Call.

/// Two-region graph: entry region runs `Call`; tail region runs `Return`.
/// The Return's ctrl input is the tail region's `ControlState`, whose
/// own control inputs trace back to the Call.
fn graph_ret_via_controlstate_after_call() -> ir::BuiltFunctionGraph {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let head = t.fb_mut().create_region().expect("head");
    t.fb_mut().set_entry_region(head).expect("entry head");
    t.fb_mut().set_region(head);

    let target = t
        .fb_mut()
        .build_int_const(0xCAFEu64, ir::node::NodeOutputType::U64);
    t.fb_mut().build_call(target).expect("call");

    let tail = t.fb_mut().create_region().expect("tail");
    t.fb_mut().build_branch(tail).expect("branch to tail");

    t.fb_mut().set_region(tail);
    t.fb_mut().build_return(None, &[]).expect("ret");

    t.finish()
}

/// Without `ignore_control_states`, `ret(call(...))` does NOT match the
/// graph above because the Return's direct ctrl predecessor is a
/// ControlState (the tail region's join), not the Call.
#[test]
fn ret_call_does_not_match_through_controlstate_by_default() {
    let g = graph_ret_via_controlstate_after_call();
    let pat: Pat = ret().preceded_by(call()).into();
    let hits = Matcher::new(&g).find_all(&pat);
    assert!(
        hits.is_empty(),
        "default matcher must not walk through ControlState; got {} hits",
        hits.len()
    );
}

/// With `ignore_control_states`, the matcher walks past the ControlState
/// and finds the Call.
#[test]
fn ret_call_matches_through_controlstate_with_ignore_control_states() {
    let g = graph_ret_via_controlstate_after_call();
    let pat: Pat = ret().preceded_by(call()).into();
    let hits = Matcher::new(&g).ignore_control_states().find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "ignore_control_states must walk through ControlState to find Call"
    );
}

/// Both flags can be combined without interference.  Pattern `add(mul, _)`
/// against `Add(ZeroExt(Mul), c)` still works when `ignore_control_states`
/// is also set.
#[test]
fn both_flags_together_do_not_interfere_with_value_walk_through() {
    let g = graph_add_zext_mul();
    let pat: Pat = add(mul(any(), any()), any()).into();
    let hits = Matcher::new(&g)
        .ignore_casts()
        .ignore_control_states()
        .find_all(&pat);
    assert_eq!(hits.len(), 1, "value walk-through still works with both flags on");
}

// ── ignore_casts_mask builder API ──────────────────────────────────────────
//
// `ignore_casts_mask(mask)` is the selective version of `ignore_casts()`:
// it sets specific cast-walk bits without enabling all of them.  Multiple
// calls union (OR-combine).  `ignore_casts()` widens to `CastMask::all()`.

/// Default: `ignore_cast_mask` is empty.
#[test]
fn matcher_default_ignore_cast_mask_is_empty() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g);
    assert!(
        m.options_for_test().ignore_cast_mask.is_empty(),
        "default ignore_cast_mask must be empty"
    );
}

/// `ignore_casts()` sets the mask to `CastMask::all()`.
#[test]
fn ignore_casts_sets_mask_to_all() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g).ignore_casts();
    assert_eq!(
        m.options_for_test().ignore_cast_mask,
        CastMask::all(),
        "ignore_casts() must set the mask to CastMask::all()"
    );
}

/// `ignore_casts_mask(TRUNCATE)` sets only the TRUNCATE bit.
#[test]
fn ignore_casts_mask_sets_just_truncate() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g).ignore_casts_mask(CastMask::TRUNCATE);
    assert_eq!(
        m.options_for_test().ignore_cast_mask,
        CastMask::TRUNCATE,
        "ignore_casts_mask(TRUNCATE) must set only the TRUNCATE bit"
    );
}

/// Two `ignore_casts_mask` calls union (OR-combine).
#[test]
fn ignore_casts_mask_unions_repeated_calls() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g)
        .ignore_casts_mask(CastMask::TRUNCATE)
        .ignore_casts_mask(CastMask::EXTEND);
    assert_eq!(
        m.options_for_test().ignore_cast_mask,
        CastMask::TRUNCATE | CastMask::EXTEND,
        "repeated ignore_casts_mask calls must union"
    );
}

/// `ignore_casts()` after `ignore_casts_mask(TRUNCATE)` widens to `all()`.
#[test]
fn ignore_casts_after_mask_widens_to_all() {
    let g = Tb::empty().ret_nothing();
    let m = Matcher::new(&g)
        .ignore_casts_mask(CastMask::TRUNCATE)
        .ignore_casts();
    assert_eq!(
        m.options_for_test().ignore_cast_mask,
        CastMask::all(),
        "ignore_casts() after a mask must widen to all()"
    );
}
