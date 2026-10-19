//! `validate` on a join with hundreds of predecessors, counted in graph edges
//! read rather than timed.
//!
//! A wide `goto out` join (an error ladder, a switch fall-in) gives one Region
//! k control inputs and one k-armed `Phi` / `MemPhi` pair. Every stage of
//! `validate` must stay linear in k; a per-arm walk of the Region's
//! predecessor list is quadratic.

use strider_ir::{Function, FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::RegisterSet;

/// `if (c) goto err;` `n` times, so `err` has `n` predecessors, one `Phi` per
/// tracked varnode and one `MemPhi`, each with `n` arms.
fn error_ladder(n: usize) -> Function {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn()
        .expect("builder with one tracked varnode");
    let rungs: Vec<_> = (0..n)
        .map(|_| b.create_region_all().expect("rung region"))
        .collect();
    let err = b.create_region_all().expect("err region");
    let done = b.create_region_all().expect("done region");
    b.set_entry_region_all(rungs[0]).expect("entry");
    for (i, &rung) in rungs.iter().enumerate() {
        b.set_region(rung);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, err, rungs.get(i + 1).copied().unwrap_or(done))
            .expect("rung terminator");
    }
    for region in [err, done] {
        b.set_region(region);
        b.build_function_return().expect("return");
    }
    b.build().expect("ladder builds")
}

fn edges_read_by_validate(n: usize) -> u64 {
    let f = error_ladder(n);
    let before = strider_graph::edge_reads();
    strider_ir::validate::validate(&f).expect("a lifted-shape ladder validates");
    strider_graph::edge_reads() - before
}

#[test]
fn validate_reads_edges_linearly_in_a_joins_predecessor_count() {
    let small = edges_read_by_validate(500);
    let large = edges_read_by_validate(4_000);
    // 8x the predecessors: linear is 8x, a per-arm walk 64x.
    assert!(
        large <= small * 12,
        "8x the predecessors read {:.1}x the edges ({small} -> {large}): \
         a validate stage is scaling with the join's width",
        large as f64 / small as f64,
    );
}
