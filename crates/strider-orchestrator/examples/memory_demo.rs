//! Renders the IR graph of `memory.elf::main` on x86_64, chosen for its mix of
//! stack stores and calls.  Post-pipeline it shows the stack-aware dot
//! rendering: a Store/Load with a `memory_offsets` entry keeps its addr-input
//! edge and gains a `base sp + K` line in its label.

use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = "fixtures/out/x64/memory.elf";
    let symbol = "main";

    let obj = strider_reader::load_elf(binary_path)?;
    let obj = obj.file();
    let mem_reader = strider_reader::ElfFileMemReader::from_object(&obj)?;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj)?;

    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    let mut strider = strider_orchestrator::Lifter::new(arch, sleigh)?;
    let cc = strider_target::CallingConvention::x86_64_systemv().build(strider.sleigh_regs())?;

    let cfg_options = strider_cfg::CfgOptions {
        allow_code_before_start_addr: true,
        ..Default::default()
    };

    let addr = obj
        .symbol_by_name(symbol)
        .ok_or_else(|| format!("'{symbol}' symbol not found in {binary_path}"))?
        .address();

    let cfg = strider.build_cfg(
        strider_cfg::MachineInsnAddr::from(addr),
        &cfg_options,
        &rustc_hash::FxHashMap::default(),
    )?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(strider.sleigh()), dot::DotStyle::dark_cfg());
    dot.dump_as_html("memory-cfg.html")?;

    let mut function = strider.build_ir(&cfg, cc)?.function;

    let dot = dot::GraphDot::new(
        function.dot_dumper(strider.sleigh())?,
        dot::DotStyle::dark(),
    );
    println!("dumping pre-opt IR graph -> memory-graph.html");
    std::fs::write("memory-graph.html", dot.as_html_from_dot()?)?;

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(Some(&rom)),
    )?;

    let dot = dot::GraphDot::new(
        function.dot_dumper(strider.sleigh())?,
        dot::DotStyle::dark(),
    );
    println!("dumping post-opt IR graph -> memory-graph-opt.html");
    std::fs::write("memory-graph-opt.html", dot.as_html_from_dot()?)?;

    println!();
    println!("open memory-graph-opt.html in a browser to see:");
    println!("  - Stack stores with [sp+OFF] labels and no addr edge");

    Ok(())
}
