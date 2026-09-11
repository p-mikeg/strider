//! Runs `Strider::analyze` on `SYMBOL` in the image passed as `argv[1]`, a
//! real heavy function in the FreeBSD 12.4 kernel it was written against, with
//! no profiler wiring, so an external sampling profiler gets a clean target.
//! Run under samply:
//!
//! ```text
//! cargo build --release --example analyze_kernel
//! samply record ./target/release/examples/analyze_kernel /path/to/kernel
//! ```

use object::{Object, ObjectSymbol};

/// Path to the image to analyze, from `argv[1]` or `$STRIDER_KERNEL`.
fn kernel_path() -> String {
    std::env::args()
        .nth(1)
        .or_else(|| std::env::var("STRIDER_KERNEL").ok())
        .unwrap_or_else(|| panic!("pass the image to analyze as argv[1], or set $STRIDER_KERNEL"))
}
const SYMBOL: &str = "x86emu_exec";

fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    let t0 = std::time::Instant::now();
    let result = strider.analyze(
        addr,
        &cc,
        &strider_orchestrator::LiftOptions::default(),
        &strider_orchestrator::opt::OptOptions::default(),
        None,
    )?;
    let elapsed = t0.elapsed();
    // Keep `result` observable so nothing is optimised away.
    println!(
        "analyzed {SYMBOL} @ {addr:#x}: {} unresolved in {elapsed:.2?}",
        result.unresolved_indirect_branches.len()
    );
    Ok(())
}
