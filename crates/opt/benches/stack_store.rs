#![allow(clippy::unwrap_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ir::node::NodeOutputType;
use ir::test_utils::sp_vn_x86 as sp_vn;
use ir::{FunctionBuilder, IntBinaryOp};
use opt::{Optimizer, StackStoreDetect};

/// Builds a straight-line `cdecl`-style function: N consecutive `push reg`
/// sequences (each is `sub esp, 4; store esp`) followed by `return`.
fn build_pushes(n: usize) -> ir::BuiltFunctionGraph {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let mut sp_v = b.read_variable(&sp).unwrap();
    let four = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
    for i in 0..n as u64 {
        sp_v = b
            .build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)
            .unwrap();
        b.write_variable(&sp, sp_v).unwrap();
        let data = b.build_int_const(i, NodeOutputType::U32).unwrap();
        b.build_store(sp_v, data, rsleigh::VnSpace::RAM).unwrap();
    }
    b.build_return(None, &[]).unwrap();
    b.build().unwrap()
}

fn bench_pushes(c: &mut Criterion) {
    let mut group = c.benchmark_group("stack_store/pushes");
    let sp = sp_vn();
    for n in [100usize, 1_000, 10_000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, &n| {
            b.iter_batched(
                || build_pushes(n),
                |mut fg| {
                    StackStoreDetect::new(sp).optimize(&mut fg.graph, fg.entry).unwrap();
                    black_box(fg);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_pushes);
criterion_main!(benches);
