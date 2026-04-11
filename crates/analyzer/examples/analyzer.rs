use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = "binary_tests/out/x86/test.elf";

    let obj = reader::load_elf(binary_path);

    // Build a short-lived ELF reader for the Sleigh context.
    let data: Vec<u8> = std::fs::read(binary_path)?;
    let data = Box::leak(data.into_boxed_slice());
    let parsed = object::File::parse(&*data)?;
    let mem_reader = reader::ElfFileMemReader::from_elf_sections(&parsed)
        .expect("failed to build ELF section reader");

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
        .symbol_by_name("fib")
        .expect("'main' symbol not found in binary")
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

    opt::default_pipeline().run(&mut function)?;

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh), dot::DotStyle::dark());
    println!("dumping opt IR graph...");
    std::fs::write("graph-opt.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph-opt.dot")?;

    Ok(())
}
