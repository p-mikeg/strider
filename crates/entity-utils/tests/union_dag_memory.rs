//! `UnionDag::for_each`'s working memory, counted rather than timed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use cranelift_entity::entity_impl;
use entity_utils::UnionDag;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// Per-thread, so the parallel test harness cannot pollute the measurement.
fn record(delta: isize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

struct PeakAlloc;

// The `GlobalAlloc` defaults route `realloc` / `alloc_zeroed` through these two.
unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            record(layout.size() as isize);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        record(-(layout.size() as isize));
        unsafe { System.dealloc(p, layout) };
    }
}

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Key(u32);
entity_impl!(Key);

const KEYS: u32 = 200_000;

/// A dense `seen` set grows a zeroed vector reaching the largest `UnionId`, so
/// one call would cost the whole arena. The highest key's closure is its own
/// node, so its walk needs a few hash-set buckets.
#[test]
fn for_each_peak_memory_does_not_scale_with_the_arena() {
    let mut dag: UnionDag<Key, u64> = UnionDag::new();
    for i in 0..KEYS {
        dag.extend(Key(i), u64::from(i));
    }
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));

    let mut yielded = 0u32;
    dag.for_each(Key(KEYS - 1), |v| {
        assert_eq!(v, u64::from(KEYS - 1));
        yielded += 1;
    });

    let peak = PEAK.with(Cell::get);
    assert_eq!(yielded, 1);
    assert!(
        peak < 1024,
        "a one-node closure peaked at {peak} bytes in a {KEYS}-node arena"
    );
}
