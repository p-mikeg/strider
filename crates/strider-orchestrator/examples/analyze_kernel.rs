//! Runs `Strider::analyze` on a real, heavy function (FreeBSD 12.4
//! `x86emu_exec`) with no profiler wiring — a clean target for an external
//! sampling profiler.  Run under samply:
//!
//! ```text
//! cargo build --release --example analyze_kernel
//! samply record ./target/release/examples/analyze_kernel
//! ```
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use object::{Object, ObjectSymbol};

const KERNEL: &str = "/mnt/c/Users/mikeg/Documents/trick_resolver/tests/freebsd/resources/kernels/freebsd/amd64/12.4/kernel";
const SYMBOL: &str = "x86emu_exec";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let obj = strider_reader::load_elf(KERNEL)?;
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
