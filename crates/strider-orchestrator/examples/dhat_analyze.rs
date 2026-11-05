//! Heap-allocation profile of `Strider::analyze` on `x86emu_exec` in the image
//! named by `argv[1]` or `$STRIDER_KERNEL` (a FreeBSD 12.4 kernel):
//!
//! ```text
//! cargo run --release --example dhat_analyze -- /path/to/kernel
//! ```
//!
//! Writes `dhat-heap.json` for dhat's `dh_view` viewer.

mod kernel;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _profiler = dhat::Profiler::new_heap();
    kernel::analyze()
}
