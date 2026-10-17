//! Peak heap live-bytes during a frontier build, counted rather than timed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use graph_algorithms::dominance::{dominance_frontiers, dominators};

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

const JOINS: u32 = 1 << 17;

/// The climb's dedup bookkeeping must not accumulate across the whole call:
/// held globally it grows to the whole `2 * JOINS`-entry frontier output,
/// where one climb needs room for a single node's frontier.
#[test]
fn frontier_build_peak_memory_is_near_its_output() {
    // `0 -> {1, 2} ->` each of `JOINS` joins, all dominated by `0`: both arms
    // carry every join, so the frontier output is `2 * JOINS` entries and the
    // climb records one pair per entry. Built before the window opens.
    let doms = dominators(0u32, |n| match n {
        0 => vec![1, 2],
        1 | 2 => (3..3 + JOINS).collect(),
        _ => Vec::new(),
    });
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));

    let fr = dominance_frontiers(&doms, |n: u32| {
        let preds: &'static [u32] = match n {
            0 => &[],
            1 | 2 => &[0],
            _ => &[1, 2],
        };
        preds.iter().copied()
    });

    let peak = PEAK.with(Cell::get);
    let entries: usize = fr.values().map(Vec::len).sum();
    assert_eq!(entries, 2 * JOINS as usize);
    let output_bytes = (entries * size_of::<u32>()) as isize;
    assert!(
        peak < 3 * output_bytes,
        "peak {peak} bytes against {output_bytes} bytes of frontier output: \
         the dedup set is held for the whole call"
    );
}
