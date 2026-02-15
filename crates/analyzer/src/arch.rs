#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endianess {
    Little,
    Big
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SleighArch {
    pub sla_spec: rsleigh::sla_spec::SlaSpec,
    pub pspec: rsleigh::pspec::PSpec,
    pub endianess: Endianess,
    pub stack_ptr_reg_name: &'static str
}

impl SleighArch {
    pub fn x86_64() -> SleighArch {
        SleighArch { 
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86_64, 
            pspec: rsleigh::pspec::PSPEC_X86_64, 
            endianess: Endianess::Little, 
            stack_ptr_reg_name: "RSP"
        }
    }

    pub fn x86() -> SleighArch {
        SleighArch { 
            sla_spec: rsleigh::sla_spec::SLA_SPEC_X86, 
            pspec: rsleigh::pspec::PSPEC_X86, 
            endianess: Endianess::Little, 
            stack_ptr_reg_name: "ESP"
        }
    }

    pub fn mipsbe32() -> SleighArch {
        SleighArch { 
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32BE, 
            pspec: rsleigh::pspec::PSPEC_MIPS32, 
            endianess: Endianess::Big, 
            stack_ptr_reg_name: "sp"
        }
    }

    pub fn mipsle32() -> SleighArch {
        SleighArch { 
            sla_spec: rsleigh::sla_spec::SLA_SPEC_MIPS32LE, 
            pspec: rsleigh::pspec::PSPEC_MIPS32, 
            endianess: Endianess::Little, 
            stack_ptr_reg_name: "sp"
        }
    }
    
    pub fn aarch64() -> SleighArch {
        SleighArch { 
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64, 
            pspec: rsleigh::pspec::PSPEC_AARCH64, 
            endianess: Endianess::Little, 
            stack_ptr_reg_name: "sp"
        }
    }

    pub fn aarchbe64() -> SleighArch {
        SleighArch { 
            sla_spec: rsleigh::sla_spec::SLA_SPEC_AARCH64BE, 
            pspec: rsleigh::pspec::PSPEC_AARCH64, 
            endianess: Endianess::Big, 
            stack_ptr_reg_name: "sp"
        }
    }
}