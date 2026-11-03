use super::{DecodeFacts, Family};

pub(crate) static AARCH64: Family = Family {
    name: "aarch64",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: false,
        internal_registers: &[
            "tmp_ldWn",
            "tmp_ldXn",
            "tmp_stWn",
            "tmp_stXn",
            "VecMemAddr",
            "VectorSelem",
            "VecRegAddr",
            "glob_mask32",
            "glob_mask64",
            "TMPZ1",
            "TMPZ2",
            "TMPZ3",
            "TMPZ4",
            "TMPZ5",
            "TMPZ6",
            "tmpCY",
            "tmpOV",
            "tmpNG",
            "tmpZR",
        ],
        volatile_registers: &[],
        // The kernel's `current`: only a context switch writes it, and a task
        // resumes with its own, so two reads in one function are one value.
        non_volatile_registers: &["sp_el0"],
    },
    rounding_ops: &["fpcrRoundingControl"],
    msr_mode_op: None,
    rounding_register: Some(("fpcrRoundingControl", "fpcr")),
};
