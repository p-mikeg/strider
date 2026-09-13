//! Lifts one fixture function stage by stage and dumps the cfg, the IR graph
//! and the optimised IR graph as `.html` and `.dot` in the working directory.
//!
//! With no argument it renders `x86/arithmetic.elf::add` to `cfg.*`, `graph.*`
//! and `graph-opt.*`. `memory` renders `x64/memory.elf::main` to the same names
//! prefixed `memory-`: a mix of stack stores and calls, whose post-pipeline
//! Store/Load labels carry a `base sp + K` line.

use object::{Object, ObjectSymbol};
use strider_target::{CallingConvention, SleighArch};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pre-built by `make -C fixtures`.
    let (binary_path, symbol, arch, cc, prefix) = match std::env::args().nth(1).as_deref() {
        None => (
            "fixtures/out/x86/arithmetic.elf",
            "add",
            SleighArch::x86(),
            CallingConvention::x86_cdecl(),
            "",
        ),
        Some("memory") => (
            "fixtures/out/x64/memory.elf",
            "main",
            SleighArch::x86_64(),
            CallingConvention::x86_64_systemv(),
            "memory-",
        ),
        Some(other) => {
            return Err(format!("unknown demo {other:?}: pass no argument or `memory`").into());
        }
    };

    let obj = strider_reader::load_elf(binary_path)?;
    let obj = obj.checked_file().expect("the mapped file is unchanged");
    let mem_reader = strider_reader::ElfFileMemReader::from_object(&obj)?;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj)?;

    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    let mut lifter = strider_orchestrator::Lifter::new(arch, sleigh)?;
    let cc = cc.build(lifter.sleigh_regs())?;

    let cfg_options = strider_cfg::CfgOptions {
        allow_code_before_start_addr: true,
        ..Default::default()
    };

    let addr = obj
        .symbol_by_name(symbol)
        .ok_or_else(|| format!("'{symbol}' symbol not found in binary {binary_path}"))?
        .address();

    let cfg = lifter.build_cfg(
        strider_cfg::MachineInsnAddr::from(addr),
        &cfg_options,
        &rustc_hash::FxHashMap::default(),
    )?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(lifter.sleigh()), dot::DotStyle::dark_cfg());
    dot.dump_as_html(format!("{prefix}cfg.html"))?;
    dot.dump_as_dot(format!("{prefix}cfg.dot"))?;

    let mut function = lifter.build_ir(&cfg, cc)?.function;

    let dot = dot::GraphDot::new(function.dot_dumper(lifter.sleigh())?, dot::DotStyle::dark());
    println!("dumping IR graph -> {prefix}graph.html");
    dot.dump_as_html(format!("{prefix}graph.html"))?;
    dot.dump_as_dot(format!("{prefix}graph.dot"))?;

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(Some(&rom)),
    )?;

    let dot = dot::GraphDot::new(function.dot_dumper(lifter.sleigh())?, dot::DotStyle::dark());
    println!("dumping opt IR graph -> {prefix}graph-opt.html");
    dot.dump_as_html(format!("{prefix}graph-opt.html"))?;
    dot.dump_as_dot(format!("{prefix}graph-opt.dot"))?;

    Ok(())
}
