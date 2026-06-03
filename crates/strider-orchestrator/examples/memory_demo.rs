#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

//! Renders the IR graph of `memory.elf::main` on x86_64 — a function rich
//! in both stack stores (`int buf[4] = {...}`, `int dst[4]`, `int x = 7`,
//! `struct point p = {1, 2}`, `union tag t`) and calls (`array_copy`,
//! `array_fill`, `array_sum`, `pointer_chase`, `struct_field_load`,
//! `struct_field_store`, `tagged_union_read`).
//!
//! After the optimizer pipeline runs, the rendered graph exercises
//! the stack-aware dot-rendering features: Store/Load nodes whose
//! addr-input edge is SUPPRESSED because their `stack_offsets`
//! side-table entry is present — the offset label (e.g. `[sp+0x10]`)
//! on the node itself replaces the redundant addr edge.

use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = "fixtures/out/x64/memory.elf";
    let symbol = "main";

    let obj = strider_reader::load_elf(binary_path)?;
    let mem_reader = strider_reader::ElfFileMemReader::from_object(&obj)?;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj)?;

    let arch = strider_target::SleighArch::x86_64();
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    let strider = strider_orchestrator::LiftDriver::new(
        arch,
        sleigh.regs()?,
        strider_target::CallingConvention::x86_64_systemv()?,
    )?;

    let cfg_options = strider_lift::cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .build();

    let addr = obj
        .symbol_by_name(symbol)
        .ok_or_else(|| format!("'{symbol}' symbol not found in {binary_path}"))?
        .address();

    let cfg =
        strider_lift::cfg::Builder::for_arch(&arch, &mut sleigh, addr, cfg_options).build()?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(&sleigh), dot::DotStyle::dark_cfg());
    dot.dump_as_html("memory-cfg.html")?;

    let mut function = strider.analyze_cfg(&cfg, &sleigh)?.function;

    let dot = dot::GraphDot::new(function.dot_dumper(&sleigh)?, dot::DotStyle::dark());
    println!("dumping pre-opt IR graph -> memory-graph.html");
    std::fs::write("memory-graph.html", dot.as_html_from_dot()?)?;

    let mut pipeline = strider.build_optimizer_pipeline();
    pipeline.add(strider_orchestrator::opt::LoadReadOnly);
    pipeline.run(
        &mut function,
        &strider_orchestrator::opt::OptCtx::with_rom(&rom),
    )?;

    let dot = dot::GraphDot::new(function.dot_dumper(&sleigh)?, dot::DotStyle::dark());
    println!("dumping post-opt IR graph -> memory-graph-opt.html");
    std::fs::write("memory-graph-opt.html", dot.as_html_from_dot()?)?;

    println!();
    println!("open memory-graph-opt.html in a browser to see:");
    println!("  - Stack stores with [sp+OFF] labels and no addr edge");

    Ok(())
}
