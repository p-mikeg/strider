//! `validate` on a join with thousands of predecessors.
//!
//! A wide `goto out` join (an error ladder, a switch fall-in) gives one Region
//! k control inputs and one k-armed `Phi` / `MemPhi` pair. Every stage of
//! `validate` must stay near-linear in k; a per-arm walk of the Region's
//! predecessor list is quadratic.

use std::time::{Duration, Instant};

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

fn time_validate(n: usize) -> Duration {
    let f = error_ladder(n);
    let start = Instant::now();
    strider_ir::validate::validate(&f).expect("a lifted-shape ladder validates");
    let elapsed = start.elapsed();
    std::hint::black_box(&f);
    elapsed
}

#[test]
fn validate_is_near_linear_in_a_joins_predecessor_count() {
    time_validate(1_000);
    let small = time_validate(2_000);
    let large = time_validate(32_000);
    // 16x the predecessors: linear is 16x, quadratic 256x. The measured
    // quadratic walk was 104x and the linear one 44x, so a bound of 80 fails
    // outright on a return to the per-arm walk while the linear walk's own
    // per-node drift does not reach it.
    assert!(
        large.as_secs_f64() < small.as_secs_f64() * 80.0,
        "16x the predecessors cost {:.1}x the validate ({small:?} -> {large:?}): \
         a validate stage is scaling with the join's width",
        large.as_secs_f64() / small.as_secs_f64(),
    );
}
