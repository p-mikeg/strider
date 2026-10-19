//! `dedup_overlapping_largest`'s working memory, counted rather than timed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

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

const N: u64 = 64_000;

fn reg(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: off,
        size,
    }
}

/// Peak heap bytes of one call, every input surviving.
fn peak_of(input: &[rsleigh::Vn]) -> isize {
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));
    let out = vn_container::dedup_overlapping_largest(input);
    assert_eq!(
        out.len(),
        input.len(),
        "nothing here strictly encloses anything"
    );
    PEAK.with(Cell::get)
}

/// Byte-identical varnodes never subsume one another (enclosure is STRICT),
/// so any state kept per survivor holds all of them, where an adjacent
/// disjoint run keeps it at one. The sweep's two scalars allocate the same
/// for both.
#[test]
fn dedup_peaks_no_higher_on_byte_identical_varnodes_than_on_disjoint_ones() {
    let identical = vec![reg(0, 4); N as usize];
    let disjoint: Vec<rsleigh::Vn> = (0..N).map(|i| reg(i * 4, 4)).collect();
    let on_identical = peak_of(&identical);
    let on_disjoint = peak_of(&disjoint);
    assert!(
        on_identical <= on_disjoint,
        "{N} byte-identical varnodes peaked at {on_identical} bytes against \
         {on_disjoint} for {N} disjoint ones: the sweep keeps per-survivor state"
    );
}
