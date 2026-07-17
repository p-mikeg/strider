//! Micro-benchmarks for `strider_pattern::Matcher` over a large synthetic
//! function (~2000 value nodes).
//!
//! Two paths are measured:
//! - `find_all` of a small pattern with many hits (the real matching path);
//! - `find_all` of a pattern whose root kind never occurs in the graph
//!   (the prefilter fast-reject path).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use strider_ir::node::ValueType;
use strider_ir::{FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::{RegisterSet, Tb};
use strider_pattern::{
    Capture, JoinConstraint, MatchPat, Matcher, add, any, any_int_const, call, if_node, int_const,
    shl,
};

/// Builds one long value chain: starting from a constant, 1000 iterations
/// alternating Add / Mul / Xor with a fresh constant each iteration.  The
/// constants vary by loop index so the dedup cache never collapses the
/// chain — the result is ~2000 distinct value nodes ending in a `Return`.
fn build_chain() -> strider_ir::Function {
    let mut t = Tb::empty();
    let mut acc = t.u64(1);
    for i in 0..1000u64 {
        let c = t.u64(0x1_0000 + i);
        acc = match i % 3 {
            0 => t.add(acc, c),
            1 => t.mul(acc, c),
            _ => t.bxor(acc, c),
        };
    }
    t.ret_val(acc)
}

/// Builds a CHAIN OF DIAMONDS: `N` guards in sequence, each with a `Call` in
/// its true arm, its false arm, and its merge region.  Nothing is optimised
/// away — the matcher runs on the raw built function, so no DCE eats the
/// filler and every call is a real hit.
///
/// The result is a deep dominator chain (the merge of diamond `i` dominates
/// every node of diamond `i+1`), which is what makes the node-vs-split tree
/// depth difference visible at all.
fn build_diamond_chain(n: u64) -> strider_ir::Function {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn().unwrap();

    let mut regions = Vec::new();
    for _ in 0..n {
        regions.push((
            b.create_region_all().unwrap(),
            b.create_region_all().unwrap(),
            b.create_region_all().unwrap(),
        ));
    }
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(regions[0].0).unwrap();

    for (i, &(head, t_arm, f_arm)) in regions.iter().enumerate() {
        // The next diamond's head is this diamond's merge target.
        let merge = regions.get(i + 1).map_or(exit, |r| r.0);
        let base = 0x1_0000 + (i as u64) * 0x100;

        b.set_region(head);
        b.set_lift_addr(Some(base));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t_arm, f_arm).unwrap();
        b.set_lift_addr(None);

        for (arm, off) in [(t_arm, 0x10), (f_arm, 0x20)] {
            b.set_region(arm);
            b.set_lift_addr(Some(base + off));
            let target = b.build_int_const(base + off, ValueType::I64).unwrap();
            b.build_call_cc(target, None).unwrap();
            b.build_branch(merge).unwrap();
            b.set_lift_addr(None);
        }
    }

    b.set_region(exit);
    b.set_lift_addr(Some(0xF_0000));
    b.build_return(None, &[]).unwrap();
    b.set_lift_addr(None);

    b.build().unwrap()
}

/// The two join shapes the single-tree change trades between:
/// - `Dominates`-ONLY: the case that could REGRESS (it now builds the bigger
///   V+E tree instead of the V one, and walks ~2x-longer chains);
/// - MIXED (`Dominates` + `DominatedByBranch`): the case that should IMPROVE
///   (one V+E build instead of two — V, then V+E).
fn join_constraint_benches(c: &mut Criterion) {
    let function = build_diamond_chain(60);

    let (g, t, cap) = (Capture::new(), Capture::new(), Capture::new());

    // Dominates-only: the If dominates every call at or after it.
    {
        let guard = if_node().capture(g).build();
        let callp = call().capture(cap).build();
        let cons = JoinConstraint::Dominates { a: g, b: cap };
        let hits = Matcher::new(&function)
            .find_joined_constrained(&[&guard, &callp], &[&cons])
            .unwrap();
        assert!(
            !hits.is_empty(),
            "dominates-only join must have hits, else this benchmark measures nothing"
        );
        c.bench_function("join_dominates_only", |b| {
            b.iter(|| {
                let hits = Matcher::new(black_box(&function))
                    .find_joined_constrained(&[&guard, &callp], &[&cons])
                    .unwrap();
                black_box(hits)
            });
        });
    }

    // Mixed: a node-dominance query AND an edge-dominance query in one join.
    {
        let guard = if_node().capture(g).capture_true(t).build();
        let callp = call().capture(cap).build();
        let dom = JoinConstraint::Dominates { a: g, b: cap };
        let branch = JoinConstraint::DominatedByBranch {
            branch: t,
            node: cap,
        };
        let hits = Matcher::new(&function)
            .find_joined_constrained(&[&guard, &callp], &[&dom, &branch])
            .unwrap();
        assert!(
            !hits.is_empty(),
            "mixed join must have hits, else this benchmark measures nothing"
        );
        c.bench_function("join_dominates_and_branch", |b| {
            b.iter(|| {
                let hits = Matcher::new(black_box(&function))
                    .find_joined_constrained(&[&guard, &callp], &[&dom, &branch])
                    .unwrap();
                black_box(hits)
            });
        });
    }
}

fn matcher_benches(c: &mut Criterion) {
    let function = build_chain();

    // ~334 Add(_, IntConst) nodes in the chain match this 3-node pattern.
    let add_pat = add(any(), any_int_const()).into_pattern();
    let hits = Matcher::new(&function).find_all(&add_pat).unwrap();
    assert!(!hits.is_empty(), "add pattern must have matches");
    c.bench_function("matcher_find_all_add_const", |b| {
        b.iter(|| {
            let hits = Matcher::new(black_box(&function))
                .find_all(&add_pat)
                .unwrap();
            black_box(hits)
        });
    });

    // No ShiftLeft node exists in the chain, so this exercises the
    // never-matching prefilter path.
    let no_match_pat = shl(int_const(0xDEAD_BEEFu128), int_const(7u128)).into_pattern();
    let hits = Matcher::new(&function).find_all(&no_match_pat).unwrap();
    assert!(hits.is_empty(), "shl pattern must never match");
    c.bench_function("matcher_find_all_no_match", |b| {
        b.iter(|| {
            let hits = Matcher::new(black_box(&function))
                .find_all(&no_match_pat)
                .unwrap();
            black_box(hits)
        });
    });
}

criterion_group!(benches, matcher_benches, join_constraint_benches);
criterion_main!(benches);
