//! The setup `analyze_kernel` and `dhat_analyze` share.

use object::{Object, ObjectSymbol};

const SYMBOL: &str = "x86emu_exec";

/// Analyzes `SYMBOL` in the image named by `argv[1]` or `$STRIDER_KERNEL`
/// and prints its address, unresolved-branch count and analysis time.
pub fn analyze() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("STRIDER_KERNEL").ok())
        .ok_or("pass the image to analyze as argv[1], or set $STRIDER_KERNEL")?;
    let elf = strider_reader::load_elf(path)?;
    let obj = elf.checked_file().expect("the mapped file is unchanged");
    let reader = || {
        strider_reader::ElfFileMemReader::new(&elf, strider_reader::elf::RelocationMode::Unapplied)
    };
    let addr = obj
        .symbol_by_name(SYMBOL)
        .ok_or_else(|| format!("'{SYMBOL}' not found"))?
        .address();

    let arch = strider_target::SleighArch::x86_64();
    let mut strider =
        strider_orchestrator::Strider::new(arch, reader()?, Some(Box::new(reader()?)))?;
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
            .count()
    );
    Ok(())
}
