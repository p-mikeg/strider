
use ir::{FunctionBuilder, IntBuilderExt};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let buf = vec![0xf3, 0xf, 0x1e, 0xfa, 0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0x89, 0x7d, 0xec, 0x83, 0x7d, 0xec, 0x4, 0x75, 0x7, 0xb8, 0x1, 0x0, 0x0, 0x0, 0xeb, 0x39, 0x90, 0x83, 0x7d, 0xec, 0x5, 0x75, 0x7, 0xb8, 0x3, 0x0, 0x0, 0x0, 0xeb, 0x2b, 0xc7, 0x45, 0xfc, 0x0, 0x0, 0x0, 0x0, 0xeb, 0x16, 0x8b, 0x45, 0xec, 0x48, 0x98, 0x48, 0x89, 0xc7, 0xb8, 0x0, 0x0, 0x0, 0x0, 0xe8, 0x0, 0x0, 0x0, 0x0, 0x83, 0x45, 0xfc, 0x1, 0x83, 0x7d, 0xfc, 0x2, 0x7e, 0xe4, 0x83, 0x6d, 0xec, 0x1, 0xeb, 0xc8, 0xc9, 0xc3];
    let memory_reader = rsleigh::mem_readers::BufMemReader::new(buf, 0x0);
    let sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64, rsleigh::pspec::PSPEC_X86_64, memory_reader)?;
    let regs = sleigh.regs()?;
    let vns: Vec<_> = vec![regs.name_to_vn("RAX").unwrap(), regs.name_to_vn("RBX").unwrap()];
    let mut builder = FunctionBuilder::new(vns.clone());
    builder.build_entry();
    let block1 = builder.create_block();
    let true_block = builder.create_block();
    let false_block = builder.create_block();
    let block2 = builder.create_block();
    builder.set_entry_block(block1);

    builder.set_block(true_block);
    let const_7 = builder.build_uint64_const(15);
    let val2 = builder.get_node_from_vn(&vns[0]);
    let sub = builder.build_int_or(const_7, val2, vns[1].size.into());
    
    builder.build_branch(block2);
    builder.set_block(block2);
    builder.build_return(Some(sub), &[]);

    builder.set_block(false_block);
    let var1 = builder.get_node_from_vn(&vns[1]);
    let const_5 = builder.build_uint64_const(5);
    builder.build_call(const_5, &vns, &vns);
    builder.build_store(var1, const_5, rsleigh::VnSpace::RAM);

    let var2 = builder.get_node_from_vn(&vns[1]);
    let load = builder.build_load(var2,  rsleigh::VnSpace::RAM, vns[0].size.into());
    builder.build_return(Some(load), &[]);

    builder.set_block(block1);
    let const_5 = builder.build_uint64_const(5);
    let var = builder.get_node_from_vn(&vns[0]);
    
    let add = builder.build_int_add(const_5, var, vns[1].size.into());
    let load = builder.build_load(add, rsleigh::VnSpace::UNIQUE, vns[0].size.into());
    
    builder.build_if(load, true_block, false_block);

    let dot = dot::GraphDot::new(builder.dot_dumper(&sleigh), dot::DotStyle::dark());

    dot.dump_as_html("graph.html")?;
    dot.dump_as_dot("graph.dot")?;
    Ok(())
}
