//! Micro-benchmarks for `strider_pattern::Matcher` over a large synthetic
//! function (~2000 value nodes).
//!
//! Two paths are measured:
//! - `find_all` of a small pattern with many hits (the real matching path);
//! - `find_all` of a pattern whose root kind never occurs in the graph
//!   (the prefilter fast-reject path).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use strider_ir_test_utils::Tb;
use strider_pattern::{MatchPat, Matcher, add, any, any_int_const, int_const, shl};

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

criterion_group!(benches, matcher_benches);
criterion_main!(benches);
