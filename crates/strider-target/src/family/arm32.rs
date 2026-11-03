use super::{DecodeFacts, Family};

pub(crate) static ARM32: Family = Family {
    name: "arm32",
    facts: DecodeFacts {
        isa_mode_var: Some("TMode"),
        flow_commit_vars: &["condit", "TEEMode"],
        // `condit` is deliberately absent: it belongs to `flow_commit_vars`, which
        // RE-DERIVES it by re-lifting the owning instruction. These are cleared
        // with no re-lift, including at a region start, which destroys an IT state
        // the predecessor legitimately committed.
        transient_decode_vars: &["LRset", "REToverride", "CALLoverride"],
        has_delay_slots: false,
        callother_output_exact: false,
        internal_registers: &[
            "mult_addr",
            "mult_dat8",
            "mult_dat16",
            "tmpNG",
            "tmpZR",
            "tmpCY",
            "tmpOV",
        ],
        volatile_registers: &[],
        non_volatile_registers: &[],
    },
    rounding_ops: &["fpscrRoundingControl"],
    msr_mode_op: Some("msrRegisterMode"),
    rounding_register: None,
};
