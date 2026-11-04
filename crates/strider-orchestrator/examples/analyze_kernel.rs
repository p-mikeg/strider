//! Runs `Strider::analyze` on `SYMBOL` in the image named by `argv[1]` or
//! `$STRIDER_KERNEL` (a FreeBSD 12.4 kernel), as a target for an external
//! sampling profiler:
//!
//! ```text
//! cargo build --profile profiling --example analyze_kernel
//! samply record ./target/profiling/examples/analyze_kernel /path/to/kernel
//! ```

use object::{Object, ObjectSymbol};

fn kernel_path() -> String {
    std::env::args()
        .nth(1)
        .or_else(|| std::env::var("STRIDER_KERNEL").ok())
        .unwrap_or_else(|| panic!("pass the image to analyze as argv[1], or set $STRIDER_KERNEL"))
}
const SYMBOL: &str = "x86emu_exec";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let elf = strider_reader::load_elf(kernel_path())?;
    let obj = elf.checked_file().expect("the mapped file is unchanged");
    let mem_reader = strider_reader::ElfFileMemReader::new(
        &elf,
        strider_reader::elf::RelocationMode::Unapplied,
    )?;
    let rom = strider_reader::ElfFileMemReader::new(
        &elf,
        strider_reader::elf::RelocationMode::Unapplied,
    )?;

    let addr = obj
        .symbol_by_name(SYMBOL)
        .ok_or_else(|| format!("'{SYMBOL}' not found"))?
        .address();

    let arch = strider_target::SleighArch::x86_64();
    let mut strider = strider_orchestrator::Strider::new(arch, mem_reader, Some(Box::new(rom)))?;
    let cc = strider_target::CallingConvention::x86_64_systemv().build(strider.sleigh_regs())?;

    let t0 = std::time::Instant::now();
    let result = strider.analyze(
        addr,
        &cc,
        &strider_orchestrator::AnalyzeOptions::default(),
        None,
    )?;
    let elapsed = t0.elapsed();
    // Keep `result` observable so nothing is optimised away.
    println!(
        "analyzed {SYMBOL} @ {addr:#x}: {} unresolved in {elapsed:.2?}",
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .collect::<Vec<_>>()
            .len()
    );
    Ok(())
}
