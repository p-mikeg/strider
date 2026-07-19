//! Micro-benchmark for the default optimizer pipeline over a large,
//! const-foldable synthetic chain (~2000 value nodes).
//!
//! `strider_ir::Function` is not `Clone`, so each iteration rebuilds the
//! input in the `iter_batched` setup closure; only `pipeline.run` is timed.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use strider_ir_test_utils::Tb;
use strider_opt::{OptCtx, default_pipeline};

/// 1000 iterations alternating Add / Mul / Xor from a constant seed. Every
/// operand is a constant, so `ConstantFold` can collapse the whole chain.
fn build_foldable_chain() -> strider_ir::Function {
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

fn pipeline_benches(c: &mut Criterion) {
    let pipeline = default_pipeline();
    c.bench_function("pipeline_fold_chain", |b| {
        b.iter_batched(
            build_foldable_chain,
            |mut function| {
                let mut ctx = OptCtx::new(None);
                pipeline.run(&mut function, &mut ctx).unwrap();
                black_box(function)
            },
            BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, pipeline_benches);
criterion_main!(benches);
