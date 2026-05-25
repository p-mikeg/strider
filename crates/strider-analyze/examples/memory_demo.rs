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
//! After AliasSplit + optimizer pipeline, the rendered graph exercises
//! every memory-related dot-rendering feature added in the recent rounds:
//!
//! * Single-node MemProject with two outputs labelled `mem:Stack` and
//!   `mem:Unknown` (#180 + #181).
//! * MemUnion barriers at each Call site re-projecting both partitions
//!   afterwards (#181).
//! * Store/Load nodes whose addr-input edge is SUPPRESSED in the render
//!   because their `stack_offsets` side-table entry is present — the
//!   offset label (e.g. `[sp+0x10]`) on the node itself replaces the
//!   redundant addr edge (#179).

use object::{Object, ObjectSymbol};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary_path = "fixtures/out/x64/memory.elf";
    let symbol = "main";

    let obj = strider_reader::load_elf(binary_path)?;
    let mem_reader = strider_reader::ElfFileMemReader::from_object(&obj)?;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj)?;

    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem_reader)?;
    let strider = strider_analyze::Strider::new(
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

    let cfg = strider_lift::cfg::Builder::for_arch(&arch, sleigh, addr, cfg_options).build()?;

    let dot = dot::GraphDot::new(cfg.dot_dumper(), dot::DotStyle::dark_cfg());
    dot.dump_as_html("memory-cfg.html")?;

    let mut function = strider.analyze_cfg(&cfg)?.graph;

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh)?, dot::DotStyle::dark());
    println!("dumping pre-opt IR graph -> memory-graph.html");
    std::fs::write("memory-graph.html", dot.as_html_from_dot()?)?;

    let mut pipeline = strider.build_optimizer_pipeline();
    pipeline.add(strider_analyze::opt::LoadReadOnly::new(std::sync::Arc::new(rom)));
    let entry = function
        .entry()
        .ok_or("memory_demo: built function missing entry")?;
    pipeline.run(&mut function, entry)?;

    let dot = dot::GraphDot::new(function.dot_dumper(&cfg.sleigh)?, dot::DotStyle::dark());
    println!("dumping post-opt IR graph -> memory-graph-opt.html");
    std::fs::write("memory-graph-opt.html", dot.as_html_from_dot()?)?;

    println!();
    println!("open memory-graph-opt.html in a browser to see:");
    println!("  - MemProject with two outputs (mem:Stack + mem:Unknown labels)");
    println!("  - MemUnion barriers at each Call site");
    println!("  - Stack stores with [sp+OFF] labels and no addr edge");
    println!("  - Per-partition memory SSA chains threading across calls");

    Ok(())
}
