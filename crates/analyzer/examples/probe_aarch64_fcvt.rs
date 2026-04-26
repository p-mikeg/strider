//! Diff IR before and after the iteration that drops FloatToFloat
use object::{Object, ObjectSymbol};
use opt::Optimizer;

fn dump_kinds(g: &ir::BuiltFunctionGraph, label: &str) {
    println!("=== {label} ===");
    let mut counts = std::collections::HashMap::new();
    for nid in g.preorder() {
        let k = format!("{:?}", g.graph.node_kind(nid));
        let key = k.split('(').next().unwrap_or("").to_string();
        *counts.entry(key).or_insert(0u32) += 1;
    }
    let mut e: Vec<_> = counts.iter().collect();
    e.sort();
    for (k, v) in e { println!("  {k}: {v}"); }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = "fixtures/out/aarch64/floats.elf";
    let obj = reader::load_elf(path)?;
    let arch = analyzer::SleighArch::aarch64();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe)?.regs()?;
    let ana = analyzer::Analyzer::new(arch, regs, analyzer::CallingConvention::aarch64_aapcs64())?;
    let mem = reader::ElfFileMemReader::from_object(&obj)?;
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem)?;
    let addr = obj.symbol_by_name("f32_to_f64").unwrap().address();
    let cfg = cfg::Builder::new(sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    ).build()?;
    let mut g = ana.analyze_cfg(&cfg)?;
    dump_kinds(&g, "iter 0");
    opt::ConstantFold.optimize(&mut g)?;
    dump_kinds(&g, "iter 1");
    opt::ConstantFold.optimize(&mut g)?;
    dump_kinds(&g, "iter 2");
    Ok(())
}
