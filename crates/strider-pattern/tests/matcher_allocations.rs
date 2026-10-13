//! The engine re-enters a pat node once per operand ordering and attempts a
//! kind-`Any` root at every reachable node, so anything it sets up per visit
//! is multiplied by `2^depth` or by the graph size. Counting heap allocations
//! rather than timing keeps the budget deterministic.
//!
//! One test in its own binary: the counter is process-wide, so a second test
//! running beside it would pollute the measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_ir::IntBinaryOp;
use strider_ir_test_utils::Tb;
use strider_pattern::matcher::{KindSpec, MatcherBuilder, PatValueRef};
use strider_pattern::{Capture, CaptureExt, MatchPat, Matcher, Pattern};

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
/// pattern below has `2^7` operand orderings.
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

/// The same shape with wildcard leaves, every ordering live and counted.
///
/// A predicate on one leaf per pair is what keeps the swap on:
/// `NodeInputs::interchangeable` would otherwise cut it at every node, two
/// bare wildcards being indistinguishable. The root capture is what keeps
/// `find_all` enumerating: binding nothing visible, the pattern would have one
/// answer per root and stop at the first configuration. Every ordering binds
/// that capture to the same value, so the match count stays 1.
fn add_tree_pattern(root: Capture, configurations: &Arc<AtomicUsize>) -> Pattern {
    let mut b = MatcherBuilder::new();
    let mut level: Vec<PatValueRef> = (0..LEAVES)
        .map(|i| {
            let leaf = b.leaf(KindSpec::Any);
            if i % 2 == 0 {
                b.set_node_predicate(leaf, Box::new(|_m, _n| true));
            }
            leaf
        })
        .collect();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(b.binary(IntBinaryOp::Add, pair[0], pair[1]));
        }
        level = next;
    }
    b.capture_output(level[0], root);
    let counter = Arc::clone(configurations);
    b.finish().with_root_post_match(Box::new(move |_, _, _, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        true
    }))
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
fn matching_allocates_per_configuration_not_per_node_visited() {
    // Exploring an operand ordering. Every ordering carries the same bindings,
    // so all but one are discarded and what is left is the bookkeeping the
    // walk down to the root set up.
    let function = balanced_add_tree();
    let configurations = Arc::new(AtomicUsize::new(0));
    let pattern = add_tree_pattern(Capture::new(), &configurations);
    let matcher = Matcher::new(&function);
    configurations.store(0, Ordering::Relaxed);
    let (hits, allocations) = allocations_of(|| {
        configurations.store(0, Ordering::Relaxed);
        matcher.find_all(&pattern).unwrap()
    });
    assert_eq!(hits.len(), 1);
    let orderings = configurations.load(Ordering::Relaxed);
    assert_eq!(orderings, 1 << (LEAVES - 1), "every ordering is explored");
    // One dedup key per configuration reaching the root, and nothing for the
    // 15 pat nodes each of them walked. The floor is that same key: below it
    // the orderings stopped reaching the root at all.
    assert!(
        (orderings..=orderings + 16).contains(&allocations),
        "{allocations} allocations over {orderings} orderings"
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
