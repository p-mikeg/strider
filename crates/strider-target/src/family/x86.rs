use super::{DecodeFacts, Family};

pub(crate) static X86: Family = Family {
    name: "x86",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: true,
        internal_registers: &["xmmTmp1", "xmmTmp2"],
        volatile_registers: &[],
        non_volatile_registers: &[],
    },
    rounding_ops: &["mxcsrRoundingControl", "x87RoundingControl"],
    msr_mode_op: None,
    rounding_register: None,
};
