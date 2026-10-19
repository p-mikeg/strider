//! What resurrecting a dead cone allocates, counted rather than timed.
//!
//! An integration test because `strider_ir_test_utils` links its own
//! compilation of `strider-ir`, and for its own global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use strider_ir::node::{IntUnaryOp, ValueId, ValueType};
use strider_ir::{EditFunction, FunctionBuilder, IRBuilderExt};
use strider_ir_test_utils::RegisterSet;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// The `GlobalAlloc` defaults route `realloc` / `alloc_zeroed` through `alloc`,
// so every byte requested passes here. Per-thread, so the parallel test
// harness cannot pollute the count.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATED.try_with(|n| n.set(n.get() + layout.size()));
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const RESURRECTIONS: u64 = 400;

/// The first resurrections size the per-node flag and queue maps to the arena,
/// once per edit session; they are not counted.
const WARM: usize = 2;

/// Bytes the resurrections after the first `WARM` allocate, with `pad` dead
/// constants ahead of them in the node arena: `RESURRECTIONS` orphan `Neg`s
/// swapped in for the values the `Return` takes.
fn resurrection_bytes(pad: u64) -> usize {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    for i in 0..pad {
        b.build_int_const(i + 0x10_0000, ValueType::int(64))
            .unwrap();
    }
    let orphans: Vec<ValueId> = (0..RESURRECTIONS)
        .map(|i| {
            let k = b.build_int_const(i, ValueType::int(64)).unwrap();
            b.build_int_unary_operation(k, IntUnaryOp::Neg, ValueType::int(64))
                .unwrap()
        })
        .collect();
    let live: Vec<ValueId> = (0..RESURRECTIONS)
        .map(|i| b.build_int_const(i + 0x1000, ValueType::int(64)).unwrap())
        .collect();
    b.build_return(None, &live).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();

    let mut ctx = EditFunction::new(&mut function);
    let mut pairs = live.iter().zip(&orphans);
    for (&dead, &orphan) in pairs.by_ref().take(WARM) {
        assert!(ctx.replace_value(dead, orphan));
    }
    let before = ALLOCATED.with(Cell::get);
    for (&dead, &orphan) in pairs {
        assert!(ctx.replace_value(dead, orphan));
    }
    ALLOCATED.with(Cell::get) - before
}

/// A resurrection costs its cone, not the arena: 100x the dead nodes ahead of
/// the same resurrections allocate the same bytes.
#[test]
fn resurrection_allocations_do_not_scale_with_the_node_arena() {
    let small = resurrection_bytes(2_000);
    let large = resurrection_bytes(200_000);
    assert!(
        large <= small + 4096,
        "100x the arena allocated {large} bytes against {small}: a resurrection \
         is scaling with the node arena"
    );
}
