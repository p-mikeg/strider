#![allow(clippy::unwrap_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ir::node::NodeOutputType;
use ir::{FunctionBuilder, IntBinaryOp};
use opt::{KnownBits, Optimizer};

fn build_or_and_chain(n: usize) -> ir::BuiltFunctionGraph {
    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut acc = b.build_int_const(0, NodeOutputType::U64);
    for i in 0..n as u64 {
        let bit = b.build_int_const(1u64 << (i % 64), NodeOutputType::U64).unwrap();
        acc = b
            .build_int_binary_operation(acc, bit, IntBinaryOp::Or, NodeOutputType::U64)
            .unwrap();
    }
    let mask = b.build_int_const(0xFFFF, NodeOutputType::U64);
    let masked = b
        .build_int_binary_operation(acc, mask, IntBinaryOp::And, NodeOutputType::U64)
        .unwrap();
    b.build_return(Some(masked), &[]).unwrap();
    b.build().unwrap()
}

fn bench_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("known_bits/or_and_chain");
    for n in [100usize, 1_000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_or_and_chain(n),
                |mut fg| {
                    let mut iters = 0usize;
                    while KnownBits.optimize(&mut fg).unwrap().changed() {
                        iters += 1;
                        if iters > 200 {
                            panic!("did not converge");
                        }
                    }
                    black_box(fg);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_chain);
criterion_main!(benches);
