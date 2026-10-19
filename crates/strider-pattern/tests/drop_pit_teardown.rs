//! The drop pit is a thread-local, so a pattern dropped from another
//! thread-local's destructor can outlive it. One test in its own binary: a
//! panic in a thread-local destructor aborts the process.
//!
//! The failing direction relies on glibc running thread-local destructors
//! last-registered first (Rust's Linux TLS registers them through
//! `__cxa_thread_atexit_impl`). Where the order differs the pit outlives
//! `HELD` and the test passes either way.

use std::cell::RefCell;

use strider_pattern::{Pattern, anything, if_else, ret};

thread_local! {
    static HELD: RefCell<Option<Pattern>> = const { RefCell::new(None) };
}

/// On glibc, thread-local destructors run last-registered first, so `HELD`,
/// registered before anything reaches the pit, is destroyed after it: its
/// pattern's branch then drops with the pit gone.
#[test]
fn a_pattern_dropped_after_the_drop_pit_is_freed_in_place() {
    std::thread::spawn(|| {
        // `HELD` registers on entry to `with`, before the pattern built
        // inside it can reach the pit.
        HELD.with(|h| *h.borrow_mut() = Some(if_else().with_true(ret().build()).build()));
        // The first trip through the pit registers its destructor.
        drop(if_else().cond(anything()));
    })
    .join()
    .expect("the thread exits cleanly");
}
