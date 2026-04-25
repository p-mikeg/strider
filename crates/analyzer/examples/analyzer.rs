#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = "fixtures/out/x86/test.elf";

    let obj = reader::load_elf(binary_path)?;
    let mem_reader = reader::ElfFileMemReader::from_object(&obj)?;
    let rom = reader::ElfFileMemReader::from_object(&obj)?;

    let arch = analyzer::SleighArch::x86();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem_reader)?;
    let analyzer = analyzer::Analyzer::new(
        arch,
        sleigh.regs()?,
        analyzer::CallingConvention::x86_cdecl(),
    )?;

    let cfg_options = cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .build();

    let addr = obj
        .symbol_by_name("struct_test")
        .ok_or("'fib' symbol not found in binary")?
        .address();

    let cfg = cfg::Builder::new(sleigh, addr, cfg_options).build()?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(), dot::DotStyle::dark_cfg());
    dot.dump_as_html("cfg.html")?;
    dot.dump_as_dot("cfg.dot")?;

    let mut function = analyzer.analyze_cfg(&cfg)?;

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh), dot::DotStyle::dark());
    println!("dumping IR graph...");
    std::fs::write("graph.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph.dot")?;

    let mut pipeline = analyzer.build_optimizer_pipeline();
    pipeline.add(opt::LoadReadOnly(rom));
    pipeline.run(&mut function)?;
    println!("dumping opt IR graph...");

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh), dot::DotStyle::dark());
    std::fs::write("graph-opt.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph-opt.dot")?;

    Ok(())
}
