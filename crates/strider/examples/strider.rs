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

    let obj = reader::load_elf(binary_path)?;
    let mem_reader = reader::ElfFileMemReader::from_object(&obj)?;
    let rom = reader::ElfFileMemReader::from_object(&obj)?;

    let arch = strider::SleighArch::x86();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    let strider = strider::Strider::new(
        arch,
        sleigh.regs()?,
        strider::CallingConvention::x86_cdecl(),
    )?;

    let cfg_options = strider_lift::cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .build();

    let addr = obj
        .symbol_by_name(symbol)
        .ok_or_else(|| format!("'{symbol}' symbol not found in binary {binary_path}"))?
        .address();

    let cfg = strider_lift::cfg::Builder::for_arch(&arch, sleigh, addr, cfg_options).build()?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(), dot::DotStyle::dark_cfg());
    dot.dump_as_html("cfg.html")?;
    dot.dump_as_dot("cfg.dot")?;

    let mut function = strider.analyze_cfg(&cfg)?.graph;

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh), dot::DotStyle::dark());
    println!("dumping IR graph...");
    std::fs::write("graph.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph.dot")?;

    let mut pipeline = strider.build_optimizer_pipeline();
    pipeline.add(strider_analyze::opt::LoadReadOnly(rom));
    pipeline.run_on_built(&mut function)?;
    println!("dumping opt IR graph...");

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh), dot::DotStyle::dark());
    std::fs::write("graph-opt.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph-opt.dot")?;

    Ok(())
}
