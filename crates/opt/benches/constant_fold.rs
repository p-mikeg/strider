#![allow(clippy::unwrap_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ir::node::NodeOutputType;
use ir::{FunctionBuilder, IntBinaryOp};
use opt::{ConstantFold, Optimizer};

fn build_chain(n: usize) -> ir::BuiltFunctionGraph {
    let vn = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            off: 0x1000,
            space: rsleigh::VnSpace::REGISTER,
        },
        size: 8,
    };
    let mut b = FunctionBuilder::new_raw(vec![vn], &[vn], &[], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut acc = b.read_variable(&vn).unwrap();
    for _ in 0..n {
        let one = b.build_int_const(1, NodeOutputType::U64);
        acc = b
            .build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
    }
    b.build_return(Some(acc), &[]).unwrap();
    b.build().unwrap()
}

fn bench_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("constant_fold/chain");
    for n in [100usize, 1_000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_chain(n),
                |mut fg| {
                    let mut iters = 0usize;
                    while ConstantFold.optimize(&mut fg).unwrap().changed() {
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
