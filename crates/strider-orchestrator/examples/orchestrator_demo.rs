#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a real fixture that ships with the workspace.  Build via
    // `make -C fixtures` if the file isn't present (the example expects
    // the fixture to be pre-built — see CLAUDE.md).
    let binary_path = "fixtures/out/x86/arithmetic.elf";
    let symbol = "add";

    let obj = strider_reader::load_elf(binary_path)?;
    let obj = obj.file();
    let mem_reader = strider_reader::ElfFileMemReader::from_object(&obj)?;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj)?;

    let arch = strider_target::SleighArch::x86();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    // The driver OWNS the Sleigh and builds the CFG itself.
    let mut strider = strider_orchestrator::Lifter::new(arch, sleigh)?;
    let cc = strider_target::CallingConvention::x86_cdecl().build(strider.sleigh_regs())?;

    let cfg_options = strider_cfg::CfgOptions {
        allow_code_before_start_addr: true,
        ..Default::default()
    };

    let addr = obj
        .symbol_by_name(symbol)
        .ok_or_else(|| format!("'{symbol}' symbol not found in binary {binary_path}"))?
        .address();

    let cfg = strider.build_cfg(
        strider_cfg::MachineInsnAddr::from(addr),
        &cfg_options,
        &rustc_hash::FxHashMap::default(),
    )?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(strider.sleigh()), dot::DotStyle::dark_cfg());
    dot.dump_as_html("cfg.html")?;
    dot.dump_as_dot("cfg.dot")?;

    let mut function = strider.build_ir(&cfg, cc)?.function;

    let dot = dot::GraphDot::new(
        function.dot_dumper(strider.sleigh())?,
        dot::DotStyle::dark(),
    );
    println!("dumping IR graph...");
    std::fs::write("graph.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph.dot")?;

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(Some(&rom)),
    )?;
    println!("dumping opt IR graph...");

    let dot = dot::GraphDot::new(
        function.dot_dumper(strider.sleigh())?,
        dot::DotStyle::dark(),
    );
    std::fs::write("graph-opt.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph-opt.dot")?;

    Ok(())
}
