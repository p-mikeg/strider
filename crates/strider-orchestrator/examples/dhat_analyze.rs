//! Heap-allocation profile of `Strider::analyze` on a real, allocation-heavy
//! function (FreeBSD 12.4 `x86emu_exec`).  Run:
//!
//! ```text
//! cargo run --release --example dhat_analyze
//! ```
//!
//! Writes `dhat-heap.json`, ranked by allocation site total bytes / blocks
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use object::{Object, ObjectSymbol};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// Path to the image to analyze, from `argv[1]` or `$STRIDER_KERNEL`.
fn kernel_path() -> String {
    std::env::args()
        .nth(1)
        .or_else(|| std::env::var("STRIDER_KERNEL").ok())
        .unwrap_or_else(|| panic!("pass the image to analyze as argv[1], or set $STRIDER_KERNEL"))
}
const SYMBOL: &str = "x86emu_exec";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _profiler = dhat::Profiler::new_heap();

    let obj = strider_reader::load_elf(kernel_path())?;
    let obj = obj.file();
    let mem_reader = strider_reader::ElfFileMemReader::from_object(&obj)?;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj)?;

    let addr = obj
        .symbol_by_name(SYMBOL)
        .ok_or_else(|| format!("'{SYMBOL}' not found"))?
        .address();

    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    let mut strider = strider_orchestrator::Strider::new(arch, sleigh, Some(Box::new(rom)))?;
    let cc = strider_target::CallingConvention::x86_64_systemv().build(strider.sleigh_regs())?;

    let result = strider.analyze(
        addr,
        &cc,
        &strider_orchestrator::LiftOptions::default(),
        &strider_orchestrator::opt::OptOptions::default(),
        None,
    )?;
    // Keep `result` observable so nothing is optimised away.
    println!(
        "analyzed {SYMBOL} @ {addr:#x}: {} unresolved",
        result.unresolved_indirect_branches.len()
    );
    Ok(())
}
