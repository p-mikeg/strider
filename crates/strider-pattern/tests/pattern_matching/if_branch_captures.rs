//! `IfPat`'s branch walk binds into the live match, so a capture shared with
//! the condition or the other branch has to agree, the capture metadata has to
//! report what the walk binds, and the walk enumerates every binding.

use strider_ir::IRViewer;
use strider_pattern::{Capture, JoinConstraint, MatchPat, Matcher, first_of, if_else, region, var};

use super::support::shapes;

/// The branch consumer is never the `If`'s condition, so a capture used in
/// both positions cannot agree and the match is rejected.
#[test]
fn shared_capture_across_cond_and_branch_must_agree() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);

    let shared = Capture::new();
    let pat = if_else()
        .cond(var(shared))
        .with_true(var(shared).into_pattern())
        .build();
    assert_eq!(m.find_all(&pat).unwrap().len(), 0);

    let a = Capture::new();
    let b = Capture::new();
    let independent = if_else()
        .cond(var(a))
        .with_true(var(b).into_pattern())
        .build();
    assert_eq!(m.find_all(&independent).unwrap().len(), 1);
}

/// The two branches have distinct consumers here, so one capture cannot cover
/// both.
#[test]
fn shared_capture_across_both_branches_must_agree() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);

    let shared = Capture::new();
    let pat = if_else()
        .with_true(var(shared).into_pattern())
        .with_false(var(shared).into_pattern())
        .build();
    assert_eq!(m.find_all(&pat).unwrap().len(), 0);

    let t = Capture::new();
    let e = Capture::new();
    let distinct = if_else()
        .with_true(var(t).into_pattern())
        .with_false(var(e).into_pattern())
        .build();
    assert_eq!(m.find_all(&distinct).unwrap().len(), 1);
}

/// A branch-only capture reaches the outer match.
#[test]
fn branch_capture_reaches_the_outer_match() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);

    let branch = Capture::new();
    let hits = m
        .find_all(&if_else().with_true(var(branch).into_pattern()).build())
        .unwrap();
    assert_eq!(hits.len(), 1);
    let node = hits[0]
        .node(branch, f.graph())
        .expect("branch capture is bound in the outer match");
    assert!(matches!(
        f.node_kind(node),
        strider_ir::node::NodeKind::Region
    ));
}

/// The branch sub-pattern's graph is not part of the enclosing pattern, so the
/// capture metadata has to be told about it: the join's range check and its
/// connectivity check both read it.
#[test]
fn branch_capture_is_reported_by_the_capture_metadata() {
    let branch = Capture::new();
    let pat = if_else().with_true(var(branch).into_pattern()).build();

    assert!(
        pat.bound_captures().any(|c| c == branch),
        "bound_captures must report a branch-bound capture"
    );
    assert!(
        pat.guaranteed_captures().unwrap().contains(&branch),
        "the branch walk must succeed for the match to, so its captures are guaranteed"
    );
}

/// A constraint over a branch-bound capture is in range: every row binds it.
#[test]
fn a_constraint_may_mention_a_branch_bound_capture() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);

    let if_cap = Capture::new();
    let branch = Capture::new();
    let pat = if_else()
        .capture(if_cap)
        .with_true(var(branch).into_pattern())
        .build();

    let rows = m
        .find_joined_constrained(
            &[&pat],
            &[JoinConstraint::Dominates {
                dominator: if_cap,
                dominated: branch,
            }],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
}

/// Two patterns correlated ONLY through a branch capture form one component.
#[test]
fn a_branch_capture_connects_two_patterns() {
    let f = shapes::if_cmp_then_return(4);
    let m = Matcher::new(&f);

    let if_cap = Capture::new();
    let branch = Capture::new();
    let guard = if_else()
        .capture(if_cap)
        .with_true(var(branch).into_pattern())
        .build();
    let target = region().capture(branch).build();

    let rows = m.find_joined_constrained(&[&guard, &target], &[]).unwrap();
    assert_eq!(rows.len(), 1);
}

/// `if (c) goto join; else goto f;` with `f` also branching to `join`, so the
/// If's true edge runs into a two-predecessor merge.
fn if_true_edge_into_a_merge() -> strider_ir::Function {
    let mut t = super::support::Tb::bare(vec![], &[], &[], &[], None, 0);
    let entry = t.region();
    let other = t.region();
    let join = t.region();
    t.set_entry(entry);

    t.enter(join);
    let ten = t.u64(10);
    t.fb_mut()
        .build_return(Some(ten), &[])
        .expect("build_return");

    t.enter(other);
    t.branch(join);

    t.enter(entry);
    let c = t.u64(4);
    let one = t.u64(1);
    let cond = t.int_cmp(c, one, strider_ir::IntCmpOp::Equal);
    t.build_if(cond, join, other);
    t.finish()
}

/// A branch sub-pattern enumerates: the same sub-pattern standalone binds the
/// merge two ways, once per predecessor, and reports both under `with_true`.
#[test]
fn a_branch_sub_pattern_reports_every_binding() {
    let f = if_true_edge_into_a_merge();
    let m = Matcher::new(&f);

    let r = Capture::new();
    let x = Capture::new();
    let sub = || region().capture(r).any_input(var(x)).build();

    let merge = f
        .graph()
        .all_node_ids()
        .find(|&n| {
            matches!(f.node_kind(n), strider_ir::node::NodeKind::Region)
                && f.node_inputs(n).len() == 2
        })
        .expect("two-predecessor merge");
    let at_merge = m
        .find_all(&sub())
        .unwrap()
        .iter()
        .filter(|hit| hit.node(r, f.graph()) == Some(merge))
        .count();
    assert_eq!(at_merge, 2);

    let embedded = m.find_all(&if_else().with_true(sub()).build()).unwrap();
    assert_eq!(embedded.len(), 2);
}

/// `if (c) goto join; else goto join;`, so both control outputs land on the
/// same two-predecessor merge.
fn if_both_edges_into_one_merge() -> strider_ir::Function {
    let mut t = super::support::Tb::bare(vec![], &[], &[], &[], None, 0);
    let entry = t.region();
    let join = t.region();
    t.set_entry(entry);

    t.enter(join);
    let ten = t.u64(10);
    t.fb_mut()
        .build_return(Some(ten), &[])
        .expect("build_return");

    t.enter(entry);
    let c = t.u64(4);
    let one = t.u64(1);
    let cond = t.int_cmp(c, one, strider_ir::IntCmpOp::Equal);
    t.build_if(cond, join, join);
    t.finish()
}

/// The false branch rejects the true branch's first binding of a shared
/// capture, which only the true branch's SECOND binding satisfies. Committing
/// to the first loses the match outright.
#[test]
fn a_later_branch_binding_can_satisfy_a_capture_the_first_one_conflicts_with() {
    let f = if_both_edges_into_one_merge();
    let m = Matcher::new(&f);

    let k = Capture::new();
    let pat = if_else()
        .with_true(region().any_input(var(k)).build())
        .with_false(region().input(1, var(k)).build())
        .build();

    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}

/// `first_of` cuts on a PRODUCED match. Inside a branch pattern, reaching the
/// end of the branch walk only hands off to the enclosing continuation, so a
/// cut there would discard the arm a later branch still needs.
#[test]
fn first_of_in_a_branch_falls_through_when_the_other_branch_rejects() {
    let f = if_both_edges_into_one_merge();
    let m = Matcher::new(&f);

    let k = Capture::new();
    let arms = || first_of![region().input(0, var(k)), region().input(1, var(k))];
    let pat = if_else()
        .with_true(arms().into_pattern())
        .with_false(region().input(1, var(k)).build())
        .build();

    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}

/// Two branches each binding two ways cross-product, and a capture shared
/// between them collapses that product to the combinations where it agrees
/// rather than binding twice.
#[test]
fn independent_branch_bindings_multiply_while_a_shared_one_agrees() {
    let f = if_both_edges_into_one_merge();
    let m = Matcher::new(&f);

    let (t, e) = (Capture::new(), Capture::new());
    let independent = if_else()
        .with_true(region().any_input(var(t)).build())
        .with_false(region().any_input(var(e)).build())
        .build();
    assert_eq!(m.find_all(&independent).unwrap().len(), 4);

    let shared = Capture::new();
    let agreeing = if_else()
        .with_true(region().any_input(var(shared)).build())
        .with_false(region().any_input(var(shared)).build())
        .build();
    assert_eq!(m.find_all(&agreeing).unwrap().len(), 2);
}
