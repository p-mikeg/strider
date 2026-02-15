use crate::{Error, Result};

fn regs_to_vns(
    reg_names: &[&str],
    sleigh_regs: &rsleigh::SleighRegs,
) -> Result<Vec<rsleigh::Vn>> {
    let sleigh_regs = sleigh_regs;

    reg_names
        .iter()
        .map(|&reg_name| {
            sleigh_regs
                .name_to_vn(reg_name)
                .ok_or(Error::UnknownRegName(reg_name.to_string()))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CallingConvention {
    arch: crate::arch::SleighArch,
    arg_passing_regs:  &'static [&'static str],
    callee_saved_regs:  &'static [&'static str],
    ret_val_regs: &'static [&'static str]
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BuiltCallingConvention {
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    pub callee_saved_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs: Vec<rsleigh::Vn>,
}


impl CallingConvention {
    pub fn x86_64_systemv_abi() -> CallingConvention {
        CallingConvention { 
            arch: crate::arch::SleighArch::x86_64(), 
            arg_passing_regs: &["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
            callee_saved_regs: &["RBX", "RSP", "RBP", "R12", "R13", "R14", "R15"],
            ret_val_regs: &["RAX", "RDX"],
         }
    }

    pub fn build(self, sleigh_regs: &rsleigh::SleighRegs) -> Result<BuiltCallingConvention> {
        Ok(BuiltCallingConvention { 
            arg_passing_regs: regs_to_vns(self.arg_passing_regs, &sleigh_regs)?, 
            callee_saved_regs: regs_to_vns(self.callee_saved_regs, &sleigh_regs)?,
            ret_val_regs: regs_to_vns(self.ret_val_regs, &sleigh_regs)?, 
        })
    }
}