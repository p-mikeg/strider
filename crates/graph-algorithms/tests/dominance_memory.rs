//! Peak heap live-bytes during a frontier build, counted rather than timed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use graph_algorithms::dominance::{DomTree, dominance_frontiers};

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

/// `0 -> {1, 2} ->` each of `JOINS` joins, all dominated by `0`: both arms
/// carry every join, so the frontier output is `2 * JOINS` entries and the
/// climb records one pair per entry.
struct WideJoin;

impl DomTree for WideJoin {
    type Node = u32;
    fn nodes(&self) -> impl Iterator<Item = u32> + '_ {
        0..3 + JOINS
    }
    fn predecessors(&self, n: u32) -> impl Iterator<Item = u32> + '_ {
        let preds: &'static [u32] = match n {
            0 => &[],
            1 | 2 => &[0],
            _ => &[1, 2],
        };
        preds.iter().copied()
    }
    fn immediate_dominator(&self, n: u32) -> Option<u32> {
        (n != 0).then_some(0)
    }
}

/// The climb's dedup bookkeeping must not accumulate across the whole call:
/// held globally it grows to the size of the frontier output, which a wide
/// join makes quadratic in node count.
#[test]
fn frontier_build_peak_memory_is_near_its_output() {
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));

    let fr = dominance_frontiers(&WideJoin, 0);

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
