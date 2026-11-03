use super::{DecodeFacts, Family};

pub(crate) static PPC: Family = Family {
    name: "ppc",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: false,
        internal_registers: &["r2Save", "RESERVE_ADDRESS"],
        // The counters among the SPRs, by number; the pspec marks only the
        // timebase. DEC 22, the 8xx breakpoint counters COUNTA/COUNTB 150/151,
        // SPURR 308, PURR 309, HDEC 310, ATBL/ATBU 526/527, UPMC1-6 771-776,
        // PMC1-8 787-794, IC 848, VTB 849; the 7xx/74xx UPMC5/6 929/930,
        // UPMC1-4 937/938/941/942, PMC5/6 945/946, PMC1-4 953/954/957/958; the
        // 40x PIT 987 and the 403 TBHI/TBLO 988/989.
        volatile_registers: &[
            "spr016", "spr096", "spr097", "spr134", "spr135", "spr136", "spr20e", "spr20f",
            "spr303", "spr304", "spr305", "spr306", "spr307", "spr308", "spr313", "spr314",
            "spr315", "spr316", "spr317", "spr318", "spr319", "spr31a", "spr350", "spr351",
            "spr3a1", "spr3a2", "spr3a9", "spr3aa", "spr3ad", "spr3ae", "spr3b1", "spr3b2",
            "spr3b9", "spr3ba", "spr3bd", "spr3be", "spr3db", "spr3dc", "spr3dd",
        ],
        non_volatile_registers: &[],
    },
    rounding_ops: &["fpscrRoundingControl"],
    msr_mode_op: None,
    rounding_register: None,
};
