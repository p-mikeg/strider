use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use strider_ir::node::ValueType;
use strider_ir::{FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::{RegisterSet, Tb};
use strider_pattern::{
    Capture, JoinConstraint, MatchPat, Matcher, any_int_const, anything, call, if_else, int_add,
    int_const, int_shl, phi,
};

/// 1000 iterations alternating Add / Mul / Xor from a constant seed. The
/// constants vary by loop index so the dedup cache can't collapse the chain:
/// ~2000 distinct value nodes ending in a `Return`.
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

/// `n` diamonds in sequence, each with a `Call` in its true arm, false arm
/// and merge region. The matcher runs on the raw built function, so no DCE
/// eats the filler and every call is a real hit.
///
/// The merge of diamond `i` dominates every node of diamond `i+1`, giving a
/// deep dominator chain; without that depth the node-vs-split tree depth
/// difference isn't visible at all.
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

fn join_constraint_benches(c: &mut Criterion) {
    let function = build_diamond_chain(60);

    let (g, t, cap) = (Capture::new(), Capture::new(), Capture::new());

    // The If dominates every call at or after it.
    {
        let guard = if_else().capture(g).build();
        let callp = call().capture(cap).build();
        let cons = JoinConstraint::Dominates {
            dominator: g,
            dominated: cap,
        };
        let hits = Matcher::new(&function)
            .find_joined_constrained(&[&guard, &callp], std::slice::from_ref(&cons))
            .unwrap();
        assert!(
            !hits.is_empty(),
            "dominates-only join must have hits, else this benchmark measures nothing"
        );
        c.bench_function("join_dominates_only", |b| {
            b.iter(|| {
                let hits = Matcher::new(black_box(&function))
                    .find_joined_constrained(&[&guard, &callp], std::slice::from_ref(&cons))
                    .unwrap();
                black_box(hits)
            });
        });
    }

    // A node-dominance query and an edge-dominance query in one join.
    {
        let guard = if_else().capture(g).capture_true(t).build();
        let callp = call().capture(cap).build();
        let dom = JoinConstraint::Dominates {
            dominator: g,
            dominated: cap,
        };
        let branch = JoinConstraint::Dominates {
            dominator: t,
            dominated: cap,
        };
        let hits = Matcher::new(&function)
            .find_joined_constrained(&[&guard, &callp], &[dom.clone(), branch.clone()])
            .unwrap();
        assert!(
            !hits.is_empty(),
            "mixed join must have hits, else this benchmark measures nothing"
        );
        c.bench_function("join_dominates_and_branch", |b| {
            b.iter(|| {
                let hits = Matcher::new(black_box(&function))
                    .find_joined_constrained(&[&guard, &callp], &[dom.clone(), branch.clone()])
                    .unwrap();
                black_box(hits)
            });
        });
    }
}

fn matcher_benches(c: &mut Criterion) {
    let function = build_chain();

    // ~334 Add(_, IntConst) nodes in the chain match this 3-node pattern.
    let add_pat = int_add(anything(), any_int_const()).into_pattern();
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

    // No ShiftLeft node exists in the chain, so only the prefilter runs.
    let no_match_pat = int_shl(int_const(0xDEAD_BEEFu128), int_const(7u128)).into_pattern();
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

/// `n` diamonds whose arms each write a distinct constant to `reg`, so every
/// merge region carries a value `Phi` (the shape
/// `JoinConstraint::PhiInputFromEdge` is about). `build_diamond_chain` writes
/// no vars and so has no phis at all.
fn build_phi_diamond_chain(n: u64) -> strider_ir::Function {
    let reg = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(reg)
        .callee_saved(reg)
        .build_fn()
        .unwrap();

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
        let merge = regions.get(i + 1).map_or(exit, |r| r.0);
        let base = 0x1_0000 + (i as u64) * 0x100;

        b.set_region(head);
        b.set_lift_addr(Some(base));
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t_arm, f_arm).unwrap();
        b.set_lift_addr(None);

        for (arm, k) in [(t_arm, 1u64), (f_arm, 2u64)] {
            b.set_region(arm);
            b.set_lift_addr(Some(base + k * 0x10));
            let v = b.build_int_const(k, ValueType::I64).unwrap();
            b.write_variable(&reg, v).unwrap();
            b.build_branch(merge).unwrap();
            b.set_lift_addr(None);
        }
    }

    b.set_region(exit);
    b.set_lift_addr(Some(0xF_0000));
    let merged = b.read_variable(&reg).unwrap();
    b.build_return(None, &[merged]).unwrap();
    b.set_lift_addr(None);

    b.build().unwrap()
}

/// `phi_input_from_edge` with the arm value bound by `any_input` on the phi
/// pattern (anchored at the phi's inputs, O(arity)) as the phi count rises.
fn phi_input_from_edge_benches(c: &mut Criterion) {
    for n in [6u64, 60] {
        let function = build_phi_diamond_chain(n);
        let (t, ph, v) = (Capture::new(), Capture::new(), Capture::new());
        let guard = if_else().capture_true(t).build();
        let phi_p = phi().any_input(int_const(v)).capture(ph).build();
        let cons = JoinConstraint::PhiInputFromEdge {
            phi: ph,
            edge: t,
            value: v,
        };
        let hits = Matcher::new(&function)
            .find_joined_constrained(&[&guard, &phi_p], std::slice::from_ref(&cons))
            .unwrap();
        assert!(
            !hits.is_empty(),
            "phi_input_from_edge join must have hits, else this benchmark measures nothing"
        );
        c.bench_function(&format!("phi_input_from_edge_any_input/{n}"), |b| {
            b.iter(|| {
                let hits = Matcher::new(black_box(&function))
                    .find_joined_constrained(&[&guard, &phi_p], std::slice::from_ref(&cons))
                    .unwrap();
                black_box(hits)
            });
        });

        // Baseline the inline form was measured against: the arm value as its
        // own free-floating root, searched over the whole graph and joined as
        // a cross-product against the phi. `any_input` must stay in the same
        // class as this, or beat it.
        let bare_phi = phi().capture(ph).build();
        let val_root = int_const(v).into_pattern();
        c.bench_function(&format!("phi_input_from_edge_floating_root/{n}"), |b| {
            b.iter(|| {
                let hits = Matcher::new(black_box(&function))
                    .find_joined_constrained(
                        &[&guard, &bare_phi, &val_root],
                        std::slice::from_ref(&cons),
                    )
                    .unwrap();
                black_box(hits)
            });
        });
    }
}

criterion_group!(
    benches,
    matcher_benches,
    join_constraint_benches,
    phi_input_from_edge_benches
);
criterion_main!(benches);
