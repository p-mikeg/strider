//! Microbenchmark for `ir::validate::validate` on synthetic large graphs.
//!
//! Targeted by Tasks 1+2 of the 2026-05-01 scaling-bottlenecks plan
//! (Layer B forward walk O(N²)→O(E); HashSet→DenseEntitySet for the
//! reachable set).
//!
//! The fixtures here exercise high-fanout (one output consumed by many
//! nodes) and high-fanin (one node consuming many inputs) shapes —
//! the regimes where the old O(E·U) Layer B forward walk degraded
//! quadratically.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use ir::validate::validate;
use ir::{Graph, IntBinaryOp};

/// Builds a graph with `n` Add nodes that all consume the same two
/// constants.  This produces a use-list of length `n` for each constant
/// output — the fan-out shape that quadratically punished the old
/// Layer B forward walk.
fn build_high_fanout(n: usize) -> (Graph, ir::node::NodeId) {
    let mut g = Graph::new();
    let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let a = g.create_node(
        NodeKind::IntConst(1u128),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let a_out = g.node_outputs(a).into_iter().next().unwrap();
    // Different constants for the second operand so each Add gets a
    // distinct (kind, inputs) tuple — CSE would otherwise collapse them.
    for k in 0..n {
        let b = g.create_node(
            NodeKind::IntConst(2u128 + k as u128),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let b_out = g.node_outputs(b).into_iter().next().unwrap();
        g.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a_out, b_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
    }
    (g, entry)
}

/// Builds a graph with `n` ControlState predecessors feeding a single
/// merge ControlState — the high-fanin shape of phi-heavy real graphs.
fn build_high_fanin(n: usize) -> (Graph, ir::node::NodeId) {
    let mut g = Graph::new();
    let entry = g.create_node(
        NodeKind::Entry,
        [],
        std::iter::repeat_n(NodeOutputKind::Control, n).collect::<Vec<_>>(),
    );
    let _mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_outs: Vec<_> = g.node_outputs(entry).into_iter().collect();
    let merge = g.create_node(
        NodeKind::ControlState,
        entry_outs,
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let merge_ctrl = g.node_outputs(merge).into_iter().next().unwrap();
    let _ret = g.create_node(NodeKind::Return, [merge_ctrl], []);
    (g, entry)
}

fn bench_high_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate/high_fanout");
    // Sample sizes shrink at the largest N because each iteration is
    // 100s of µs — Criterion's default sample-size still completes in
    // ≤30 s.  Add 100K to surface where O(N²) regressions would
    // otherwise hide behind sub-second measurements.
    for n in [100usize, 1_000, 10_000, 100_000].iter() {
        let (graph, entry) = build_high_fanout(*n);
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, _| {
            b.iter(|| {
                let _ = validate(black_box(&graph), black_box(entry));
            });
        });
    }
    group.finish();
}

fn bench_high_fanin(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate/high_fanin");
    for n in [100usize, 1_000, 10_000, 100_000].iter() {
        let (graph, entry) = build_high_fanin(*n);
        group.bench_with_input(BenchmarkId::from_parameter(n), n, |b, _| {
            b.iter(|| {
                let _ = validate(black_box(&graph), black_box(entry));
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_high_fanout, bench_high_fanin);
criterion_main!(benches);
