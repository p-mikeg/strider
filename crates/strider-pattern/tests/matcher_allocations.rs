//! The engine re-enters a pat node once per operand ordering and attempts a
//! kind-`Any` root at every reachable node, so anything it sets up per attempt
//! is multiplied by `2^depth` or by the graph size. Counting heap allocations
//! rather than timing keeps the budget deterministic.
//!
//! One test in its own binary: the counter is process-wide, so a second test
//! running beside it would pollute the measurement.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_ir::IntBinaryOp;
use strider_ir_test_utils::Tb;
use strider_pattern::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use strider_pattern::{CaptureExt, MatchPat, Matcher, Pattern};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every method forwards to `System` with the arguments it was given.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const LEAVES: usize = 8;
const CHAIN: usize = 400;

/// Runs `f` once to warm the kind index, then counts what a second run
/// allocates.
fn allocations_of<T>(f: impl Fn() -> T) -> (T, usize) {
    drop(f());
    let before = ALLOCS.load(Ordering::Relaxed);
    let out = f();
    (out, ALLOCS.load(Ordering::Relaxed) - before)
}

/// `return(((1+2)+(3+4))+((5+6)+(7+8)))`: seven commutative adds, so the
/// pattern below has `2^7` operand orderings at the root.
fn balanced_add_tree() -> strider_ir::Function {
    let mut t = Tb::empty();
    let mut level: Vec<strider_ir::node::ValueId> =
        (0..LEAVES).map(|i| t.u64(i as u64 + 1)).collect();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(t.add(pair[0], pair[1]));
        }
        level = next;
    }
    t.ret_val(level[0])
}

/// The same shape with wildcard leaves.
fn add_tree_pattern() -> Pattern {
    let mut b = MatcherBuilder::new();
    let mut level: Vec<PatValueRef> = (0..LEAVES).map(|_| b.leaf(KindSpec::Any)).collect();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(b.binary(IntBinaryOp::Add, pair[0], pair[1]));
        }
        level = next;
    }
    b.finish()
}

/// `return(1+2+3+...)`, left-associated, wide enough that a per-candidate
/// allocation would dominate.
fn add_chain(n: usize) -> strider_ir::Function {
    let mut t = Tb::empty();
    let mut acc = t.u64(1);
    for i in 0..n {
        let c = t.u64(i as u64 + 2);
        acc = t.add(acc, c);
    }
    t.ret_val(acc)
}

#[test]
fn matching_allocates_per_match_not_per_attempt() {
    // Exploring an operand ordering. A capture-free pattern collapses every
    // ordering into one match, so what is left is the per-attempt bookkeeping.
    let function = balanced_add_tree();
    let pattern = add_tree_pattern();
    let matcher = Matcher::new(&function);
    let (hits, allocations) = allocations_of(|| matcher.find_all(&pattern).unwrap());
    assert_eq!(hits.len(), 1);
    assert!(
        allocations <= 16,
        "{allocations} allocations for one match over 2^7 orderings"
    );

    // Scanning a candidate. A kind-`Any` root is attempted at every reachable
    // node, and a rejecting node predicate stops each attempt before it can
    // bind anything.
    let function = add_chain(CHAIN);
    let matcher = Matcher::new(&function);
    let pattern = strider_pattern::anything()
        .filter(|_m, _n| false)
        .into_pattern();
    let (hits, allocations) = allocations_of(|| matcher.find_all(&pattern).unwrap());
    assert!(hits.is_empty());
    assert!(
        allocations <= 16,
        "{allocations} allocations while scanning {CHAIN} adds"
    );
}
