use super::{DecodeFacts, Family};

pub(crate) static MIPS: Family = Family {
    name: "mips",
    facts: DecodeFacts {
        // The low bit selects the alternate ISA, which `RELP` fixes: every MIPS
        // pspec here pins it to 1, so that ISA is MIPS16e. rsleigh ships
        // `PSPEC_MIPS32MICRO` / `PSPEC_MIPS64MICRO` for `RELP=0` (microMIPS) and no
        // preset uses them, so a microMIPS image decodes its odd-addressed
        // functions against MIPS16e tables; Python's `load_elf` refuses one.
        isa_mode_var: Some("ISA_MODE"),
        flow_commit_vars: &[],
        // `ext_delay` is written by the five MIPS16e branch/call constructors
        // (`mips16.sinc:520,528,542,550,557`), reachable only at `ISA_MODE=1`.
        transient_decode_vars: &["PAIR_INSTRUCTION_FLAG", "ext_delay"],
        has_delay_slots: true,
        callother_output_exact: false,
        internal_registers: &["tsp"],
        // CP0 `Count` is a cycle counter, `Random` a TLB index that steps every
        // cycle, `Cause` holds the pending-interrupt bits the hardware sets,
        // `PerfCnt.1`/`.3`/`.5`/`.7` are the performance counters (the even
        // selects are their controls) and `cop0_reg9.6` Octeon's `CvmCount`.
        // No MIPS pspec marks any of them.
        volatile_registers: &[
            "Count",
            "Random",
            "Cause",
            "PerfCnt.1",
            "PerfCnt.3",
            "PerfCnt.5",
            "PerfCnt.7",
            "cop0_reg9.6",
        ],
        non_volatile_registers: &[],
    },
    rounding_ops: &["fcsrRoundingControl"],
    msr_mode_op: None,
    rounding_register: None,
};
