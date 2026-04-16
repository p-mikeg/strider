use ir::{FunctionBuilder, IntBinaryOp};
use ir::node::NodeOutputType;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let buf = vec![
        0xf3, 0x0f, 0x1e, 0xfa, 0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20,
        0x89, 0x7d, 0xec, 0x83, 0x7d, 0xec, 0x04, 0x75, 0x07, 0xb8, 0x01, 0x00,
        0x00, 0x00, 0xeb, 0x39, 0x90, 0x83, 0x7d, 0xec, 0x05, 0x75, 0x07, 0xb8,
        0x03, 0x00, 0x00, 0x00, 0xeb, 0x2b, 0xc7, 0x45, 0xfc, 0x00, 0x00, 0x00,
        0x00, 0xeb, 0x16, 0x8b, 0x45, 0xec, 0x48, 0x98, 0x48, 0x89, 0xc7, 0xb8,
        0x00, 0x00, 0x00, 0x00, 0xe8, 0x00, 0x00, 0x00, 0x00, 0x83, 0x45, 0xfc,
        0x01, 0x83, 0x7d, 0xfc, 0x02, 0x7e, 0xe4, 0x83, 0x6d, 0xec, 0x01, 0xeb,
        0xc8, 0xc9, 0xc3,
    ];
    let memory_reader = rsleigh::mem_readers::BufMemReader::new(buf, 0x0);
    let sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        memory_reader,
    )?;
    let regs = sleigh.regs()?;
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let vns = vec![rax, rbx];

    // FunctionBuilder::new takes (all_vars, arg_regs, callee_saved, ret_regs).
    // build_entry() is called automatically inside new().
    let mut builder = FunctionBuilder::new(vns.clone(), &[], &[rbx], &[rax])?;

    // Create regions (formerly "blocks").
    let entry_region = builder.create_region()?;
    let true_region  = builder.create_region()?;
    let false_region = builder.create_region()?;
    let merge_region = builder.create_region()?;

    builder.set_entry_region(entry_region)?;

    // ── true branch ──────────────────────────────────────────────────────────
    builder.set_region(true_region);
    let const_15 = builder.build_uint64_const(15);
    let rax_val  = builder.read_variable(&rax)?;
    let or_result = builder.build_int_binary_operation(
        const_15, rax_val, IntBinaryOp::Or, NodeOutputType::U64,
    )?;
    builder.build_branch(merge_region)?;

    // ── merge (return) region ─────────────────────────────────────────────────
    builder.set_region(merge_region);
    builder.build_return(Some(or_result), &[])?;

    // ── false branch ──────────────────────────────────────────────────────────
    builder.set_region(false_region);
    let rbx_val  = builder.read_variable(&rbx)?;
    let const_5  = builder.build_uint64_const(5);
    // build_call takes only the call-target address output.
    builder.build_call(const_5)?;
    builder.build_store(rbx_val, const_5, rsleigh::VnSpace::RAM)?;
    let load = builder.build_load(rbx_val, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    builder.build_return(Some(load), &[])?;

    // ── entry region: compute condition and branch ────────────────────────────
    builder.set_region(entry_region);
    let const_5b = builder.build_uint64_const(5);
    let rax_val2 = builder.read_variable(&rax)?;
    let add = builder.build_int_binary_operation(
        const_5b, rax_val2, IntBinaryOp::Add, NodeOutputType::U64,
    )?;
    let load2 = builder.build_load(add, rsleigh::VnSpace::UNIQUE, NodeOutputType::U64)?;
    builder.build_if(load2, true_region, false_region)?;

    // Finalise the graph and dump it.
    let function = builder.build()?;
    let dot = dot::GraphDot::new(function.dot_dumper(&sleigh), dot::DotStyle::dark());

    std::fs::write("graph.html", dot.as_html_from_dot()?)?;
    dot.dump_as_dot("graph.dot")?;
    Ok(())
}
