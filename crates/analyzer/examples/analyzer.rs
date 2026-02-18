use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let buf = include_bytes!("../../../binary_tests/binary").to_vec();
    let reader = rsleigh::mem_readers::BufMemReader::new(buf, 0x0);

    // let obj = reader::load_elf("binary_tests/vmlinux");

    // let reader: reader::ElfFileMemReader<'_, '_> = reader::ElfFileMemReader::from_elf_segments(&obj).expect("");

    let arch = analyzer::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)?;
    let analyzer = analyzer::Analyzer::new(arch, sleigh.regs()?, analyzer::CallingConvention::x86_64_systemv_abi())?;

    let regs = sleigh.regs()?;
    for reg in regs.iter() {
        if reg.vn.size > 16 {
            println!("{} {}", reg.name, reg.vn.size);
        }
    }

    let cfg_options = cfg::OptionsBuilder::new().allow_code_before_start_addr().build();
    let addr = 0; // obj.symbol_by_name("update_srbds_msr").expect("msg").address();
    let cfg = cfg::Builder::new(sleigh, addr, cfg_options).build()?;
    let dot = dot::GraphDot::new(
        cfg.dot_dumper(),
        dot::DotStyle::dark(),
    );

    dot.dump_as_html("cfg.html")?;
    dot.dump_as_dot("cfg.dot")?;

    let function = analyzer.analyze_cfg(&cfg)?;


    let dot = dot::GraphDot::new(
        function.dot_dumper(&cfg.sleigh),
        dot::DotStyle::dark(),
    );
    println!("dumping\n");

    dot.dump_as_html("graph.html")?;
    dot.dump_as_dot("graph.dot")?;

    // let mut optimizer = opt::OptimizerPipeline::new();
    // optimizer.add(opt::RedundantSelectors);
    // optimizer.run(&mut function);

    //     let dot = dot::GraphDot::new(
    //     function.dot_dumper(&cfg.sleigh),
    //     dot::DotStyle::dark(),
    // );
    // println!("dumping\n");

    // dot.dump_as_html("graph_after.html")?;
    // dot.dump_as_dot("graph_after.dot")?;
    Ok(())
}