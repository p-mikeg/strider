#![allow(clippy::unwrap_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use strider_ir::node::NodeOutputType;
use strider_ir::{FunctionBuilder, IntBinaryOp};
use opt::default_pipeline;

fn build_mixed(n: usize) -> strider_ir::BuiltFunctionGraph {
    let vn = rsleigh::Vn {
        addr_off: 0x1000,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut acc = b.read_variable(&vn).unwrap();
    for _ in 0..n {
        let one = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
        acc = b
            .build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
    }
    b.build_return(Some(acc), &[]).unwrap();
    b.build().unwrap()
}

fn bench_default(c: &mut Criterion) {
    let mut group = c.benchmark_group("default_pipeline/mixed");
    // Sample size scales down for the largest N (each iter is seconds);
    // Criterion still produces statistically significant reports.
    group.sample_size(20);
    for n in [100usize, 1_000, 10_000, 100_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_mixed(n),
                |mut fg| {
                    default_pipeline().run(&mut fg.graph, fg.entry).unwrap();
                    black_box(fg);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_default);
criterion_main!(benches);
