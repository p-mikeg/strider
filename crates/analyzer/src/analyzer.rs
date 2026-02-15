use std::{collections::HashMap};

use ir::{BoolBuilderExt, IntBuilderExt, Builder};
use rsleigh::Opcode;

use crate::error::{Error, Result};


pub struct IrAnalyzer<'a, R: rsleigh::MemReader> {
    pub(crate) analyzer: &'a Analyzer,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) region_to_block: HashMap<cfg::RegionId, ir::BlockId>,
    pub(crate) cfg: &'a cfg::Cfg<R>
}

impl<'a, R: rsleigh::MemReader>IrAnalyzer<'a, R> {

    fn new(analyzer: &'a Analyzer, cfg: &'a cfg::Cfg<R>) -> Self {
        // Find all variables
        let all_vns = analyzer.find_all_unique_vns(cfg);
    
        // Create the builder to create the ir graph
        let builder = ir::FunctionBuilder::new(all_vns);

        Self {
            analyzer,
            builder,
            region_to_block: HashMap::new(),
            cfg
        }
    }

    // The problem with choosing a register to load / store is that some of them are contained in other
    // for example al,ah in rax - we want to always return rax and then trunc it to the correct mask
    fn find_largest_fitting_register(&self, reg: &rsleigh::Vn) -> Option<rsleigh::Vn> {
        assert_eq!(reg.addr.space, rsleigh::VnSpace::REGISTER);
        let reg_start = reg.addr.off;
        let reg_end = reg_start + reg.size as u64;
        let mut largest_reg_container: Option<rsleigh::Vn> = None;
        for sleigh_reg in self.builder.variables() {
            if !matches!(sleigh_reg.addr.space, rsleigh::VnSpace::REGISTER) {
                continue;
            }
            let sleigh_reg_start = sleigh_reg.addr.off;
            let sleigh_reg_end = sleigh_reg_start + sleigh_reg.size as u64;

            if sleigh_reg_start > reg_start {
                continue;
            }
            if sleigh_reg_end < reg_end {
                continue;
            }
            // We know now that the reg is contained by sleigh reg
            if let Some(reg_container) = largest_reg_container {
                // If the current container is larger - choose it
                if reg_container.size < sleigh_reg.size { 
                    largest_reg_container = Some(*sleigh_reg);
                }
            } else {
                largest_reg_container = Some(*sleigh_reg);
            }
        }
        largest_reg_container
    }

    fn calculate_reg_shift_from_container(&self, reg: &rsleigh::Vn, container_reg: &rsleigh::Vn) -> u64 {
        match self.analyzer.arch.endianess {
            crate::arch::Endianess::Little => {
                8 * (reg.addr.off - container_reg.addr.off)
            },
            crate::arch::Endianess::Big => {
                8 * (container_reg.size as u64 - (reg.addr.off - container_reg.addr.off))
            }
        }
    }

    fn read_reg_vn(&mut self, reg: &rsleigh::Vn) -> ir::Value {
        let container_reg = self.find_largest_fitting_register(reg).expect("Must have a container for a reg (itself at least)");
        let curr_reg_val = self.builder.get_node_from_vn(&container_reg);
        let mut read_reg_val = curr_reg_val;
        if container_reg != *reg {
            // We need to shift the value if it is in the middle of a register
            let shift_value =  self.calculate_reg_shift_from_container(reg, &container_reg);
            if shift_value != 0 {
                let shift_const = self.builder.build_int_const(shift_value, container_reg.size.into());
                read_reg_val = self.builder.build_int_shift_right(curr_reg_val, shift_const, reg.size.into())
            }
        }

        read_reg_val
    }

    fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: ir::Value) {
        let container_reg = self.find_largest_fitting_register(reg).expect("Must have a container for a reg (itself at least)");
        if container_reg == *reg {
            self.builder.update_var(reg, val);
            return;
        }
        let container_reg_val = self.builder.get_node_from_vn(&container_reg);


        // The register is in the part of a bigger container
        let shift_bits =  self.calculate_reg_shift_from_container(reg, &container_reg);

        // Calculate the shifted value that should be in the reg inside the container
        let shifted_value = if shift_bits == 0 {
            self.builder.build_int_zextend(val, container_reg.size.into())
        } else {
            let shift_const = self.builder.build_int_const(shift_bits, container_reg.size.into());
            self.builder.build_int_shift_left(val, shift_const, reg.size.into())
        };

        // Calculate the masked value of the reg in the container
        let reg_mask = crate::utils::vn_mask(reg);
        let reg_mask_val = self.builder.build_int_const(reg_mask, container_reg.size.into());
        let reg_val = self.builder.build_int_and(reg_mask_val, shifted_value, container_reg.size.into());

        // Calculate the rest of the container
        let container_mask = crate::utils::vn_mask(&container_reg) & (!reg_mask);
        let container_mask_val = self.builder.build_int_const(container_mask, container_reg.size.into());
        let container_val = self.builder.build_int_and(container_mask_val, container_reg_val, container_reg.size.into());
        
        // Merge the containers
        let final_container_value = self.builder.build_int_or(container_val, reg_val, container_reg.size.into());
        self.write_reg_vn(&container_reg, final_container_value);
    }

    fn read_vn(&mut self, vn: &rsleigh::Vn) -> ir::Value {
        let default_code_space = self.cfg.sleigh.default_code_space();
        let space = vn.addr.space;
        match space {
            rsleigh::VnSpace::CONST => self.builder.build_int_const(vn.addr.off, vn.size.into()),
            rsleigh::VnSpace::UNIQUE => self.builder.get_node_from_vn(vn),
            space if space == default_code_space => {
                let space_info = self.cfg.sleigh.space_info(space);
                let addr = self.builder.build_int_const(vn.addr.off, space_info.addr_size().into());
                self.builder.build_load(addr, space, vn.size.into())
            }
            rsleigh::VnSpace::REGISTER => self.read_reg_vn(vn),
            _ => unreachable!()
        }
    }

    fn write_vn(&mut self, vn: &rsleigh::Vn, val: ir::Value) {
        let default_code_space = self.cfg.sleigh.default_code_space();
        let space = vn.addr.space;
        match space {
            rsleigh::VnSpace::CONST => unreachable!(),
            rsleigh::VnSpace::UNIQUE => self.builder.update_var(&vn, val),
            space if space == default_code_space => {
                let space_info = self.cfg.sleigh.space_info(space);
                let addr = self.builder.build_int_const(vn.addr.off, space_info.addr_size().into());
                self.builder.build_store(addr, val, space)
            }
            rsleigh::VnSpace::REGISTER => self.write_reg_vn(vn, val),
            _ => unreachable!()
        }
    }

    fn build_entry(&mut self) {
        self.builder.build_entry()
    }

    fn create_ir_blocks(&mut self) {
        // Clear state if there was something previously
        self.region_to_block.clear();
        for region_id in self.cfg.region_ids() {
            self.region_to_block.insert(region_id, self.builder.create_block());
        }
    }

    fn set_ir_entry_block(&mut self) -> Result<()>{
        let entry_block = self.region_to_block.get(&self.cfg.entry).ok_or(Error::CfgNoRegion(self.cfg.entry))?;
        self.builder.set_entry_block(*entry_block);
        Ok(())
    }

    fn iterate_region_block(&self) -> impl Iterator<Item = (cfg::RegionId, ir::BlockId)> {
        self.region_to_block.iter().map(|(k, v)| (*k, *v))
    }

    fn set_curr_ir_block(&mut self, block: ir::BlockId) {
        self.builder.set_block(block);
    }

    fn region_insn(&mut self, region_id: cfg::RegionId) -> Result<Vec<rsleigh::Insn>> {
        self.cfg.region_insn(region_id).map_err(|e| Error::CfgError(e))
    }

    fn process_unary_op(&mut self, insn: &rsleigh::Insn, 
        mut f: impl FnMut(&mut ir::FunctionBuilder, ir::Value, ir::ValueType) -> ir::Value,
    ) {
        let input = self.read_vn(&insn.inputs[0]);
        let out_vn = insn.output.as_ref().expect("binary op must have an output");
        let out = f(&mut self.builder, input, out_vn.size.into());
        self.write_vn(out_vn, out);
    }

    fn process_binary_op(&mut self, insn: &rsleigh::Insn, 
        mut f: impl FnMut(&mut ir::FunctionBuilder, ir::Value, ir::Value, ir::ValueType) -> ir::Value,
    ) {
        let lhs = self.read_vn(&insn.inputs[0]);
        let rhs = self.read_vn(&insn.inputs[1]);
        let out_vn = insn.output.as_ref().expect("binary op must have an output");
        let out = f(&mut self.builder, lhs, rhs, out_vn.size.into());
        self.write_vn(out_vn, out);
    }

    fn process_insn(&mut self, region_id: cfg::RegionId, insn: &rsleigh::Insn) {
        match insn.opcode {
            Opcode::Nop => (),
            Opcode::BoolNeg => self.process_unary_op(insn, 
                |builder, a, _size| builder.build_boolean_neg(a) 
            ),
            Opcode::BoolAnd => self.process_binary_op(insn, 
                |builder, a, b, _size| builder.build_boolean_and(a, b) 
            ),
            Opcode::BoolOr => self.process_binary_op(insn, 
                |builder, a, b, _size| builder.build_boolean_or(a, b) 
            ),
            Opcode::BoolXor => self.process_binary_op(insn, 
                |builder, a, b, _size| builder.build_boolean_xor(a, b) 
            ),
            Opcode::Int2Comp => self.process_unary_op(insn, 
                |builder, a, size| builder.build_int_not(a, size) 
            ),
            Opcode::IntNeg => self.process_unary_op(insn, 
                |builder, a, size| builder.build_int_neg(a, size) 
            ),
            Opcode::IntAdd => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_add(a, b, size) 
            ),
            Opcode::IntAnd => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_and(a, b, size) 
            ),
            Opcode::IntXor => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_xor(a, b, size) 
            ),
            Opcode::IntOr => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_or(a, b, size) 
            ),
            Opcode::IntDiv => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_div(a, b, size) 
            ),
            Opcode::IntSdiv => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_sdiv(a, b, size) 
            ),
            Opcode::IntMul => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_mul(a, b, size) 
            ),
            Opcode::IntRight => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_shift_right(a, b, size) 
            ),
            Opcode::IntSright => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_sshift_right(a, b, size) 
            ),
            Opcode::IntLeft => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_shift_left(a, b, size) 
            ),
            Opcode::IntCarry => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_carry(a, b, size) 
            ),
            Opcode::IntEqual => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_equal(a, b, size) 
            ),
            Opcode::IntLess => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_less(a, b, size) 
            ),
            Opcode::IntSless => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_sless(a, b, size) 
            ),
            Opcode::IntLessEqual => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_less_equal(a, b, size) 
            ),
            Opcode::IntRem => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_rem(a, b, size) 
            ),
            Opcode::IntSrem => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_srem(a, b, size) 
            ),
            Opcode::IntScarry => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_scarry(a, b, size) 
            ),
            Opcode::IntSborrow => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_sborrow(a, b, size) 
            ),
            Opcode::IntSlessEqual => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_sless_equal(a, b, size) 
            ),
            Opcode::IntSub => self.process_binary_op(insn, 
                |builder, a, b, size| builder.build_int_sub(a, b, size) 
            ),
            Opcode::IntSext => self.process_unary_op(insn, 
                |builder, a, size| builder.build_int_sextend(a, size) 
            ),
            Opcode::IntZext => self.process_unary_op(insn, 
                |builder, a, size| builder.build_int_zextend(a, size) 
            ),
            Opcode::IntNotEqual => {
                // We treat not equal as neg(equal) for determenstic results
                let lhs = self.read_vn(&insn.inputs[0]);
                let rhs = self.read_vn(&insn.inputs[1]);
                let out_vn = insn.output.as_ref().expect("binary op must have an output");
                let and_out = self.builder.build_int_and(lhs, rhs, out_vn.size.into());
                let out = self.builder.build_boolean_neg(and_out);
                self.write_vn(out_vn, out);
            },
            Opcode::Branch => {
                let branch_region = self.cfg.region_branch(region_id).expect("branch region id not found");
                let dest_block = self.region_to_block.get(&branch_region).expect("branch block id not found");
                self.builder.build_branch(*dest_block);
            },
            Opcode::CondBranch => {
                let cond = self.read_vn(&insn.inputs[1]);
                let res = self.cfg.region_if(region_id);
                let if_true_region = res.if_true_region.expect("region if true not found");
                let if_false_region = res.if_false_region.expect("region if true not found");
                let true_block = self.region_to_block.get(&if_true_region).expect("block if true not found");
                let false_block = self.region_to_block.get(&if_false_region).expect("block if false not found");

                self.builder.build_if(cond, *true_block, *false_block);
            },
            Opcode::Copy => {
                let input = self.read_vn(&insn.inputs[0]);
                let out_vn = insn.output.as_ref().expect("copy op must have an output");
                self.write_vn(out_vn, input);
            },
            Opcode::Load => {
                let space = insn.inputs[0].addr.space;
                let addr = self.read_vn(&insn.inputs[1]);
                let out_vn = insn.output.as_ref().expect("load op must have an output");
                let out = self.builder.build_load(addr, space, out_vn.size.into());
                self.write_vn(out_vn, out);
            },
            Opcode::Store => {
                let space = insn.inputs[0].addr.space;
                let addr = self.read_vn(&insn.inputs[1]);
                let data = self.read_vn(&insn.inputs[2]);
                self.builder.build_store(addr, data, space);
            },
            Opcode::Return => {
                let regs: Vec<_> = self.builder.variables().copied()
                    .filter(|vn| vn.addr.space == rsleigh::VnSpace::REGISTER).collect();
                if insn.inputs.len() > 0 {
                    let ret_val = self.read_vn(&insn.inputs[0]);
                    self.builder.build_return(Some(ret_val), &regs);
                } else {
                    self.builder.build_return(None, &regs);
                }
            },
            Opcode::Popcount => {
                let input = self.read_vn(&insn.inputs[0]);
                let out = self.builder.build_int_popcount(input, insn.inputs[0].size.into());
                let out_vn = insn.output.as_ref().expect("popcount op must have an output");
                self.write_vn(out_vn, out);
            }
            Opcode::Call | Opcode::CallIndirect => {
                let call_address = self.read_vn(&insn.inputs[0]);
                self.builder.build_call(call_address, 
                    &self.analyzer.calling_convention.arg_passing_regs, 
                    &self.analyzer.calling_convention.callee_saved_regs);
            },
            Opcode::CallOther => {},
            _ =>  unimplemented!("{:?}", insn.opcode)
        }
    }


}

pub struct Analyzer {
    calling_convention: crate::calling_convention::BuiltCallingConvention,
    arch: crate::SleighArch
}

impl Analyzer {
    pub fn new(arch: crate::SleighArch, sleigh_regs: rsleigh::SleighRegs, calling_convention: crate::CallingConvention) -> Result<Self> {
        let built_calling_convention = calling_convention.build(&sleigh_regs)?;
        Ok(Self {
            arch,
            calling_convention: built_calling_convention,
        })
    }

    fn find_all_unique_vns<R: rsleigh::MemReader>(&self, cfg: &cfg::Cfg<R>) -> Vec<rsleigh::Vn>  {
        let mut all_vns: std::collections::HashSet<rsleigh::Vn> = std::collections::HashSet::new();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    all_vns.insert(vn);
                }
            }
        }
        all_vns.iter().copied().collect()
    }

    pub fn analyze_cfg<R: rsleigh::MemReader>(&self, cfg: &cfg::Cfg<R>) -> Result<ir::FunctionBody>{
        let mut ir_analyzer = IrAnalyzer::new(self, cfg);

        // Build the entry for the graph
        ir_analyzer.build_entry();

        // Create all ir blocks
        ir_analyzer.create_ir_blocks();

        // Set entry block
        ir_analyzer.set_ir_entry_block()?;

        let region_blocks: Vec<_> = ir_analyzer.iterate_region_block().collect();
        let fallthroughs: Vec<_> = cfg.iterate_fallthroughs().collect();

        // process instructions
        for (region_id, block_id) in region_blocks {
            ir_analyzer.set_curr_ir_block(block_id);
            let insns = ir_analyzer.region_insn(region_id)?;

            for insn in insns {
                ir_analyzer.process_insn(region_id, &insn)
            }
        }

        // All blocks except fallthrough are linked
        for (region_parent_id, region_child_id) in fallthroughs {
            let block_parent_id = ir_analyzer.region_to_block.get(&region_parent_id).unwrap();
            let block_child_id = ir_analyzer.region_to_block.get(&region_child_id).unwrap();
            ir_analyzer.builder.link_blocks(*block_parent_id, *block_child_id);
        }
        Ok(ir_analyzer.builder.body().to_owned())
    }
}