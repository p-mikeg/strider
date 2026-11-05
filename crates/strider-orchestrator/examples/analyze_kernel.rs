//! Runs `Strider::analyze` on `x86emu_exec` in the image named by `argv[1]`
//! or `$STRIDER_KERNEL` (a FreeBSD 12.4 kernel), as a target for an external
//! sampling profiler:
//!
//! ```text
//! cargo build --profile profiling --example analyze_kernel
//! samply record ./target/profiling/examples/analyze_kernel /path/to/kernel
//! ```

mod kernel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    kernel::analyze()
}
