use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};
use CallOtherClass::NoOp;

pub(crate) static PPC: Family = Family {
    #[cfg(any(test, feature = "test-util"))]
    name: "ppc",
    #[cfg(any(test, feature = "test-util"))]
    ghidra_ldefs: "PowerPC/data/languages/ppc.ldefs",
    facts: DecodeFacts {
        isa_mode_var: None,
        flow_commit_vars: &[],
        transient_decode_vars: &[],
        has_delay_slots: false,
        callother_output_exact: false,
        register_space_reach: None,
        internal_registers: &["r2Save", "RESERVE_ADDRESS"],
        // The counter SPRs (DEC, PURR, HDEC, the PMCs, PIT, ...); the pspec
        // marks only the timebase.
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
    call_other: CALL_OTHER,
    call_other_prefixes: PREFIXES,
    call_other_fallback: None,
    calling_conventions: CALLING_CONVENTIONS,
};

/// PowerPC `wrtee` / `wrteei`, which write MSR.EE; `mfmsr` reads it back.
const PPC_MSR: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["MSR"],
    clobbers_memory: false,
    no_return: false,
});

/// FPSCR.FPCC, which a VSX compare or data-class test sets as it sets CR
/// field BF; `mcrfs` and `mffs` read it back.
const PPC_FPCC: CallOtherClass = writes(FPCC_VX.split_at(4).0);

/// [`PPC_FPCC`] plus the invalid-operation bits and summaries a
/// quad-precision compare raises (VXSNAN, and VXVC for `xscmpoqp`).
const PPC_FPCC_VX: CallOtherClass = writes(FPCC_VX);

#[rustfmt::skip]
const FPCC_VX: &[&str] = &[
    "fp_cc0", "fp_cc1", "fp_cc2", "fp_cc3",
    "fp_fx", "fp_fex", "fp_vx", "fp_vxsnan", "fp_vxvc",
];

/// The FPSCR status bits a scalar VSX arithmetic, rounding or conversion op
/// may set: FPRF (`fp_c`, FPCC), FR, FI, the exceptions and their summaries.
const PPC_FP_RESULT: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &[
        "fp_fx",
        "fp_fex",
        "fp_vx",
        "fp_ox",
        "fp_ux",
        "fp_zx",
        "fp_xx",
        "fp_vxsnan",
        "fp_vxisi",
        "fp_vxidi",
        "fp_vxzdz",
        "fp_vximz",
        "fp_vxsqrt",
        "fp_vxcvi",
        "fp_fr",
        "fp_fi",
        "fp_c",
        "fp_cc0",
        "fp_cc1",
        "fp_cc2",
        "fp_cc3",
    ],
    clobbers_memory: false,
    no_return: false,
});

/// A VSX vector conversion: the exceptions it raises and their summaries,
/// with FPRF, FR and FI left alone.
const PPC_FP_VECTOR: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &[
        "fp_fx",
        "fp_fex",
        "fp_vx",
        "fp_ox",
        "fp_ux",
        "fp_xx",
        "fp_vxsnan",
    ],
    clobbers_memory: false,
    no_return: false,
});

/// A VSX compare writing XT, or a max/min: VXSNAN, the ordered compares'
/// VXVC, and their summaries.
const PPC_FP_INVALID: CallOtherClass = writes(FP_INVALID_CR6.split_at(FP_INVALID_CR6.len() - 1).0);

/// [`PPC_FP_INVALID`] plus the CR6 an `Rc=1` vector compare sets.
const PPC_FP_INVALID_CR6: CallOtherClass = writes(FP_INVALID_CR6);

const FP_INVALID_CR6: &[&str] = &["fp_fx", "fp_fex", "fp_vx", "fp_vxsnan", "fp_vxvc", "cr6"];

/// Every FPSCR bit `mtfsf` can rewrite (`FM=0xff`), which `setSummaryFPSCR`
/// and `mcrfs` read back directly.
const PPC_FPSCR: CallOtherClass = writes(FPSCR_CR1.split_at(FPSCR_CR1.len() - 1).0);

/// `mtfsf.` / `mtfsfi.`: the FPSCR write plus the CR1 the `Rc=1` form sets.
const PPC_FPSCR_CR1: CallOtherClass = writes(FPSCR_CR1);

#[rustfmt::skip]
const FPSCR_CR1: &[&str] = &[
    "fp_fx", "fp_fex", "fp_vx", "fp_ox", "fp_ux", "fp_zx", "fp_xx", "fp_vxsnan",
    "fp_vxisi", "fp_vxidi", "fp_vxzdz", "fp_vximz", "fp_vxvc", "fp_fr", "fp_fi", "fp_c",
    "fp_cc0", "fp_cc1", "fp_cc2", "fp_cc3", "fp_reserve1", "fp_vxsoft", "fp_vxsqrt", "fp_vxcvi",
    "fp_ve", "fp_oe", "fp_ue", "fp_ze", "fp_xe", "fp_ni", "fp_rn0", "fp_rn1",
    "cr1",
];

/// A register-only op writing `w`.
const fn writes(w: &'static [&'static str]) -> CallOtherClass {
    CallOtherClass::Call(CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: w,
        clobbers_memory: false,
        no_return: false,
    })
}

/// An AltiVec record form (`vcmp*.`, `bcd*.`): the sla leaves its CR6
/// unwritten. A non-record `vcmp*` shares the op name; over-approximated.
const PPC_CR6: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["cr6"],
    clobbers_memory: false,
    no_return: false,
});

/// `slbfee.` / `icswx.`: the sla emits no `cr0flags`.
const PPC_DOT_CR0: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["cr0"],
    clobbers_memory: false,
    no_return: false,
});

/// [`PPC_DOT_CR0`] for a dot form that also has external memory effects.
const PPC_DOT_CR0_MEM: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["cr0"],
    clobbers_memory: true,
    no_return: false,
});

/// Linux PowerPC `sc`: r0 = number, r3..r8 = args, r3 = return; r0, r4-r12,
/// cr0, CTR and XER are volatile. The XER bits `adde` and friends use are the
/// separate `xer_*` registers.
const PPC_SYSCALL: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &["r0", "r3", "r4", "r5", "r6", "r7", "r8"],
    implicit_writes: &[
        "r0",
        "r3",
        "r4",
        "r5",
        "r6",
        "r7",
        "r8",
        "r9",
        "r10",
        "r11",
        "r12",
        "cr0",
        "CTR",
        "XER",
        "xer_so",
        "xer_ov",
        "xer_ov32",
        "xer_ca",
        "xer_ca32",
        "xer_count",
    ],
    clobbers_memory: true,
    no_return: false,
});

const CALL_OTHER: &[Row] = &[
    Row::new(&["returnFromInterrupt"], CallOtherClass::NO_RETURN),
    // `tw` / `td` take their condition as an operand, so control falls
    // through; an unconditional one is `trap_is_unconditional`.
    Row::new(&["trapDoubleWord"], MEM_CLOBBER),
    Row::new(&["trapDoubleWordImmediate"], MEM_CLOBBER),
    Row::new(&["trapWord"], MEM_CLOBBER),
    // VSX forms taking the base register as a value: their memory access has
    // no p-code.
    Row::new(&["vsx207_1"], MEM_CLOBBER),  // lxsiwax
    Row::new(&["vsx207_2"], MEM_CLOBBER),  // lxsiwzx
    Row::new(&["vsx207_3"], MEM_CLOBBER),  // lxsspx
    Row::new(&["vsx300_10"], MEM_CLOBBER), // lxvll
    Row::new(&["vsx300_11"], MEM_CLOBBER), // lxvwsx
    Row::new(&["vsx300_15"], MEM_CLOBBER), // stxsd
    Row::new(&["vsx300_16"], MEM_CLOBBER), // stxsibx
    Row::new(&["vsx300_17"], MEM_CLOBBER), // stxsihx
    Row::new(&["vsx300_18"], MEM_CLOBBER), // stxssp
    Row::new(&["vsx300_2"], MEM_CLOBBER),  // lxsibzx
    Row::new(&["vsx300_20"], MEM_CLOBBER), // stxvb16x
    Row::new(&["vsx300_21"], MEM_CLOBBER), // stxvh8x
    Row::new(&["vsx300_22"], MEM_CLOBBER), // stxvl
    Row::new(&["vsx300_23"], MEM_CLOBBER), // stxvll
    Row::new(&["vsx300_3"], MEM_CLOBBER),  // lxsihzx
    Row::new(&["vsx300_4"], MEM_CLOBBER),  // lxssp
    Row::new(&["vsx300_7"], MEM_CLOBBER),  // lxvb16x
    Row::new(&["vsx300_8"], MEM_CLOBBER),  // lxvh8x
    Row::new(&["vsx300_9"], MEM_CLOBBER),  // lxvl
    // The compares and data-class tests set FPSCR.FPCC alongside CR field BF,
    // which the sla writes from the op; the quad-precision compares also
    // raise VXSNAN, `xscmpoqp` VXVC.
    Row::new(&["vsx300_29"], PPC_FPCC),    // xscmpexpdp
    Row::new(&["vsx300_30"], PPC_FPCC),    // xscmpexpqp
    Row::new(&["vsx300_34"], PPC_FPCC_VX), // xscmpoqp
    Row::new(&["vsx300_35"], PPC_FPCC_VX), // xscmpuqp
    Row::new(&["vsx300_74"], PPC_FPCC),    // xststdcdp
    Row::new(&["vsx300_75"], PPC_FPCC),    // xststdcqp
    Row::new(&["vsx300_76"], PPC_FPCC),    // xststdcsp
    // Arithmetic, rounding and conversion forms set FPSCR status bits.
    // `xscmpnedp` and `xvcmpne{dp,sp}[.]` are draft encodings, given
    // `xscmpeqdp`'s and `xvcmpeqdp[.]`'s footprints.
    Row::new(&["vsx207_11"], PPC_FP_RESULT),      // xsaddsp
    Row::new(&["vsx207_14"], PPC_FP_RESULT),      // xscvsxdsp
    Row::new(&["vsx207_15"], PPC_FP_RESULT),      // xscvuxdsp
    Row::new(&["vsx207_16"], PPC_FP_RESULT),      // xsdivsp
    Row::new(&["vsx207_17"], PPC_FP_RESULT),      // xsmaddasp
    Row::new(&["vsx207_18"], PPC_FP_RESULT),      // xsmaddmsp
    Row::new(&["vsx207_19"], PPC_FP_RESULT),      // xsmsubasp
    Row::new(&["vsx207_20"], PPC_FP_RESULT),      // xsmsubmsp
    Row::new(&["vsx207_21"], PPC_FP_RESULT),      // xsmulsp
    Row::new(&["vsx207_22"], PPC_FP_RESULT),      // xsnmaddasp
    Row::new(&["vsx207_23"], PPC_FP_RESULT),      // xsnmaddmsp
    Row::new(&["vsx207_24"], PPC_FP_RESULT),      // xsnmsubasp
    Row::new(&["vsx207_25"], PPC_FP_RESULT),      // xsnmsubmsp
    Row::new(&["vsx207_26"], PPC_FP_RESULT),      // xsresp
    Row::new(&["vsx207_27"], PPC_FP_RESULT),      // xsrsp
    Row::new(&["vsx207_28"], PPC_FP_RESULT),      // xsrsqrtesp
    Row::new(&["vsx207_29"], PPC_FP_RESULT),      // xssqrtsp
    Row::new(&["vsx207_30"], PPC_FP_RESULT),      // xssubsp
    Row::new(&["vsx300_26"], PPC_FP_RESULT),      // xsaddqp
    Row::new(&["vsx300_27"], PPC_FP_RESULT),      // xsaddqpo
    Row::new(&["vsx300_28"], PPC_FP_INVALID),     // xscmpeqdp
    Row::new(&["vsx300_31"], PPC_FP_INVALID),     // xscmpgedp
    Row::new(&["vsx300_32"], PPC_FP_INVALID),     // xscmpgtdp
    Row::new(&["vsx300_33"], PPC_FP_INVALID),     // xscmpnedp
    Row::new(&["vsx300_37"], PPC_FP_RESULT),      // xscvdphp
    Row::new(&["vsx300_38"], PPC_FP_RESULT),      // xscvdpqp
    Row::new(&["vsx300_39"], PPC_FP_RESULT),      // xscvhpdp
    Row::new(&["vsx300_40"], PPC_FP_RESULT),      // xscvqpdp
    Row::new(&["vsx300_41"], PPC_FP_RESULT),      // xscvqpdpo
    Row::new(&["vsx300_42"], PPC_FP_RESULT),      // xscvqpsdz
    Row::new(&["vsx300_43"], PPC_FP_RESULT),      // xscvqpswz
    Row::new(&["vsx300_44"], PPC_FP_RESULT),      // xscvqpudz
    Row::new(&["vsx300_45"], PPC_FP_RESULT),      // xscvqpuwz
    Row::new(&["vsx300_46"], PPC_FP_RESULT),      // xscvsdqp
    Row::new(&["vsx300_47"], PPC_FP_RESULT),      // xscvudqp
    Row::new(&["vsx300_48"], PPC_FP_RESULT),      // xsdivqpo
    Row::new(&["vsx300_51"], PPC_FP_RESULT),      // xsmaddqp
    Row::new(&["vsx300_52"], PPC_FP_RESULT),      // xsmaddqpo
    Row::new(&["vsx300_53"], PPC_FP_INVALID),     // xsmaxcdp
    Row::new(&["vsx300_54"], PPC_FP_INVALID),     // xsmaxjdp
    Row::new(&["vsx300_55"], PPC_FP_INVALID),     // xsmincdp
    Row::new(&["vsx300_56"], PPC_FP_INVALID),     // xsminjdp
    Row::new(&["vsx300_57"], PPC_FP_RESULT),      // xsmsubqp
    Row::new(&["vsx300_58"], PPC_FP_RESULT),      // xsmsubqpo
    Row::new(&["vsx300_59"], PPC_FP_RESULT),      // xsmulqp
    Row::new(&["vsx300_60"], PPC_FP_RESULT),      // xsmulqpo
    Row::new(&["vsx300_63"], PPC_FP_RESULT),      // xsnmaddqp
    Row::new(&["vsx300_64"], PPC_FP_RESULT),      // xsnmaddqpo
    Row::new(&["vsx300_65"], PPC_FP_RESULT),      // xsnmsubqp
    Row::new(&["vsx300_66"], PPC_FP_RESULT),      // xsnmsubqpo
    Row::new(&["vsx300_67"], PPC_FP_RESULT),      // xsrqpi
    Row::new(&["vsx300_68"], PPC_FP_RESULT),      // xsrqpix
    Row::new(&["vsx300_69"], PPC_FP_RESULT),      // xsrqpxp
    Row::new(&["vsx300_70"], PPC_FP_RESULT),      // xssqrtqp
    Row::new(&["vsx300_71"], PPC_FP_RESULT),      // xssqrtqpo
    Row::new(&["vsx300_72"], PPC_FP_RESULT),      // xssubqp
    Row::new(&["vsx300_73"], PPC_FP_RESULT),      // xssubqpo
    Row::new(&["vsx300_81"], PPC_FP_INVALID),     // xvcmpnedp
    Row::new(&["vsx300_82"], PPC_FP_INVALID_CR6), // xvcmpnedp.
    Row::new(&["vsx300_83"], PPC_FP_INVALID),     // xvcmpnesp
    Row::new(&["vsx300_84"], PPC_FP_INVALID_CR6), // xvcmpnesp.
    Row::new(&["vsx300_85"], PPC_FP_VECTOR),      // xvcvhpsp
    Row::new(&["vsx300_86"], PPC_FP_VECTOR),      // xvcvsphp
    Row::new(&["vsx300_xsdivqp"], PPC_FP_RESULT), // xsdivqp, a fork op
    Row::new(
        &[
            "vectorCompareBoundsFloatingPoint",
            "vectorCompareEqualToFloatingPoint",
            "vectorCompareEqualToUnsignedByte",
            "vectorCompareEqualToUnsignedHalfWord",
            "vectorCompareEqualToUnsignedWord",
            "vectorCompareGreaterThanOrEqualToFloatingPoint",
            "vectorCompareGreaterThanFloatingPoint",
            "vectorCompareGreaterThanSignedByte",
            "vectorCompareGreaterThanConditionRegisterSignedHalfWord",
            "vectorCompareGreaterThanSignedWord",
            "vectorCompareGreaterThanUnsignedByte",
            "vectorCompareGreaterThanUnsignedHalfWord",
            "vectorCompareGreaterThanUnsignedWord",
        ],
        PPC_CR6,
    ),
    Row::new(&["altv207_14", "altv207_16", "altv207_18"], PPC_CR6), // vcmp{equd,gtsd,gtud}.
    Row::new(&["altv207_64", "altv207_65"], PPC_CR6),               // bcdadd. bcdsub.
    // bcdcfn. .. bcdutrunc.
    Row::new(
        &[
            "altv300_1",
            "altv300_2",
            "altv300_3",
            "altv300_4",
            "altv300_5",
            "altv300_6",
            "altv300_7",
            "altv300_8",
            "altv300_9",
            "altv300_10",
            "altv300_12",
            "altv300_13",
            "altv300_14",
        ],
        PPC_CR6,
    ),
    // vcmpne{b,h,w}. vcmpnez{b,h,w}.
    Row::new(
        &[
            "altv300_21",
            "altv300_23",
            "altv300_25",
            "altv300_27",
            "altv300_29",
            "altv300_31",
        ],
        PPC_CR6,
    ),
    // Store-conditional and byte-reverse stores (their STORE is p-code; the
    // clobber over-approximates); dcbz zeroes a block; dcbf/dcbst/icbi and
    // TLB/SLB management are ordering barriers; icswx/copy-paste and doorbell
    // messages have external memory effects.
    Row::new(&["MessageClear"], MEM_CLOBBER),
    Row::new(&["MessageSend"], MEM_CLOBBER),
    Row::new(&["StoreDoublewordByteReverseIndexed"], MEM_CLOBBER),
    Row::new(&["TLBInvalidateEntry"], MEM_CLOBBER),
    Row::new(&["TLBInvalidateEntryLocal"], MEM_CLOBBER),
    Row::new(&["TLBSynchronize"], MEM_CLOBBER),
    Row::new(&["TLBWrite"], MEM_CLOBBER),
    Row::new(&["copytrans"], MEM_CLOBBER),
    Row::new(&["dataCacheBlockClearToZero"], MEM_CLOBBER),
    Row::new(&["dataCacheBlockFlush"], MEM_CLOBBER),
    Row::new(&["dataCacheBlockInvalidate"], MEM_CLOBBER),
    Row::new(&["dataCacheBlockStore"], MEM_CLOBBER),
    Row::new(&["icswxDotOp"], PPC_DOT_CR0_MEM),
    Row::new(&["instructionCacheBlockInvalidate"], MEM_CLOBBER),
    Row::new(&["instructionCacheCongruenceClassInvalidate"], MEM_CLOBBER),
    Row::new(&["message"], MEM_CLOBBER),
    Row::new(&["pastetrans"], MEM_CLOBBER),
    Row::new(&["slbInvalidateAll"], MEM_CLOBBER),
    Row::new(&["slbMoveToEntry"], MEM_CLOBBER),
    Row::new(&["storeDoubleWordConditionalIndexed"], MEM_CLOBBER),
    Row::new(&["storeWordConditionalIndexed"], MEM_CLOBBER),
    Row::new(&["syscall"], PPC_SYSCALL),
    // A low-power wait, like ARM WFI.
    Row::new(&["waitT"], MEM_CLOBBER),
    // `ldbrx` reads inside the op.
    Row::new(&["LoadDoublewordByteReverseIndexed"], MEM_CLOBBER),
    // `lvsl` and the hardware RNG.
    Row::new(&["loadVectorForShiftLeft"], PURE),
    Row::new(&["random"], PURE),
    // e500 `mfpmr` / `mtpmr`; a `CallOther` is never deduplicated, so two
    // counter reads stay two values.
    Row::new(&["moveFromPerformanceMonitorRegister"], PURE),
    Row::new(&["moveToPerformanceMonitorRegister"], PURE),
    // The FR, FI, OX and UX bits of the scalar float ops the sla spells in
    // p-code, which writes them from these ops' outputs.
    Row::new(&["floatAddInexact"], PURE),
    Row::new(&["floatAddOverflow"], PURE),
    Row::new(&["floatAddRoundedUp"], PURE),
    Row::new(&["floatAddUnderflow"], PURE),
    Row::new(&["floatDivInexact"], PURE),
    Row::new(&["floatDivOverflow"], PURE),
    Row::new(&["floatDivRoundedUp"], PURE),
    Row::new(&["floatDivUnderflow"], PURE),
    Row::new(&["floatMaddInexact"], PURE),
    Row::new(&["floatMaddOverflow"], PURE),
    Row::new(&["floatMaddRoundedUp"], PURE),
    Row::new(&["floatMaddUnderflow"], PURE),
    Row::new(&["floatMulInexact"], PURE),
    Row::new(&["floatMulOverflow"], PURE),
    Row::new(&["floatMulRoundedUp"], PURE),
    Row::new(&["floatMulUnderflow"], PURE),
    Row::new(&["floatSqrtInexact"], PURE),
    Row::new(&["floatSqrtRoundedUp"], PURE),
    Row::new(&["floatSubInexact"], PURE),
    Row::new(&["floatSubOverflow"], PURE),
    Row::new(&["floatSubRoundedUp"], PURE),
    Row::new(&["floatSubUnderflow"], PURE),
    Row::new(&["floatToIntInexact"], PURE),
    Row::new(&["floatToIntRoundedUp"], PURE),
    Row::new(&["intToFloatInexact"], PURE),
    Row::new(&["intToFloatRoundedUp"], PURE),
    // The FPRF a conversion to an integer leaves undefined (a fork op).
    Row::new(&["fpscrUndefined"], PURE),
    // `slbmfee` / `slbmfev` / `slbfee.` read the SLB into the op's output.
    Row::new(&["slbMoveFromEntryESID"], PURE),
    Row::new(&["slbMoveFromEntryVSID"], PURE),
    Row::new(&["slbfeeDotOp"], PPC_DOT_CR0),
    // Void pcodeops writing FPSCR bits or MSR, which the sla reads back as
    // registers (`setSummaryFPSCR`, `mfmsr`). `MoveToFPSCRField*` are only
    // the ppc64 `L=1` / `W=1` forms of `mtfsf`, which is otherwise p-code.
    Row::new(&["MoveToFPSCRFieldImmediate"], PPC_FPSCR),
    Row::new(&["MoveToFPSCRFieldImmediate1"], PPC_FPSCR_CR1),
    Row::new(&["MoveToFPSCRFields"], PPC_FPSCR),
    Row::new(&["MoveToFPSCRFields1"], PPC_FPSCR_CR1),
    Row::new(&["WriteExternalEnable"], PPC_MSR),
    Row::new(&["WriteExternalEnableImmediate"], PPC_MSR),
    // Prefetch hints and a branch-history clear.
    Row::new(&["clearHistory"], NoOp),
    Row::new(&["dataCacheBlockTouch"], NoOp),
    Row::new(&["dataCacheBlockTouchForStore"], NoOp),
    Row::new(&["dataStreamStopAll"], NoOp),
];

/// AltiVec / VSX / named vector ops (`altv207_<n>`, `vsx<ver>_<n>`,
/// `vector<Op>`) that `CALL_OTHER` does not name: register compute whose
/// destination is the op's output.
const PREFIXES: &[Prefix] = &[Prefix {
    prefixes: &["altv", "vsx", "vector"],
    class: PURE,
}];

const CALLING_CONVENTIONS: &[CcPreset] = &[
    CcPreset {
        name: "powerpc_sysv32",
        cc: CallingConvention::powerpc_sysv32(),
        arches: &[ArchPreset::Ppc32Be, ArchPreset::Ppc32Le],
        ghidra: &[(
            "PowerPC/data/languages/ppc_32.cspec",
            None,
            &[ArchPreset::Ppc32Be, ArchPreset::Ppc32Le],
        )],
    },
    // A kernel flavour takes its userland prototype.
    CcPreset {
        name: "powerpc_sysv32_linux_kernel",
        cc: CallingConvention::powerpc_sysv32_linux_kernel(),
        arches: &[ArchPreset::Ppc32Be, ArchPreset::Ppc32Le],
        ghidra: &[(
            "PowerPC/data/languages/ppc_32.cspec",
            None,
            &[ArchPreset::Ppc32Be, ArchPreset::Ppc32Le],
        )],
    },
    CcPreset {
        name: "powerpc64_elf_v1",
        cc: CallingConvention::powerpc64_elf_v1(),
        arches: &[ArchPreset::Ppc64Be],
        ghidra: &[(
            "PowerPC/data/languages/ppc_64_be.cspec",
            None,
            &[ArchPreset::Ppc64Be],
        )],
    },
    CcPreset {
        name: "powerpc64_elf_v1_linux_kernel",
        cc: CallingConvention::powerpc64_elf_v1_linux_kernel(),
        arches: &[ArchPreset::Ppc64Be],
        ghidra: &[(
            "PowerPC/data/languages/ppc_64_be.cspec",
            None,
            &[ArchPreset::Ppc64Be],
        )],
    },
    // Ghidra gives a big-endian ELFv2 image `ppc_64_be.cspec`, so ELFv2 is
    // held to both ppc64 files.
    CcPreset {
        name: "powerpc64_elf_v2",
        cc: CallingConvention::powerpc64_elf_v2(),
        arches: &[ArchPreset::Ppc64Be, ArchPreset::Ppc64Le],
        ghidra: &[
            (
                "PowerPC/data/languages/ppc_64_le.cspec",
                None,
                &[ArchPreset::Ppc64Le],
            ),
            (
                "PowerPC/data/languages/ppc_64_be.cspec",
                None,
                &[ArchPreset::Ppc64Be],
            ),
        ],
    },
    CcPreset {
        name: "powerpc64_elf_v2_linux_kernel",
        cc: CallingConvention::powerpc64_elf_v2_linux_kernel(),
        arches: &[ArchPreset::Ppc64Be, ArchPreset::Ppc64Le],
        ghidra: &[
            (
                "PowerPC/data/languages/ppc_64_le.cspec",
                None,
                &[ArchPreset::Ppc64Le],
            ),
            (
                "PowerPC/data/languages/ppc_64_be.cspec",
                None,
                &[ArchPreset::Ppc64Be],
            ),
        ],
    },
];

/// The `*_linux_kernel` float windows: the kernel is built
/// `-mno-altivec -mno-vsx`, so no vector register carries a value.
const PPC32_SCALAR_ARGS_F: &[&str] = &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"];
const PPC64_SCALAR_ARGS_F: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
];
const PPC_SCALAR_RET_F: &[&str] = &["f1", "f2", "f3", "f4"];

/// `powerpc64_elf_v1`'s callee-saved list without r13: the kernel's
/// per-CPU `paca`, which a call that sleeps can return on another CPU.
const PPC64_KERNEL_SAVED: &[&str] = &[
    "r2", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24", "r25",
    "r26", "r27", "r28", "r29", "r30", "r31", "LR", "f14", "f15", "f16", "f17", "f18", "f19",
    "f20", "f21", "f22", "f23", "f24", "f25", "f26", "f27", "f28", "f29", "f30", "f31", "cr2",
    "cr3", "cr4", "vs52", "vs53", "vs54", "vs55", "vs56", "vs57", "vs58", "vs59", "vs60", "vs61",
    "vs62", "vs63",
];

impl CallingConvention {
    /// The PowerPC 32-bit System V ABI calling convention. The
    /// `-mbss-plt` GOT thunk (`bl _GLOBAL_OFFSET_TABLE_-4`, a `blrl` word)
    /// needs a per-address convention leaving `LR` out of the callee-saved
    /// registers, or `mflr` after it reads the `bl`'s return address.
    #[must_use]
    pub const fn powerpc_sysv32() -> Self {
        // r2 is the TLS pointer and r13 the small-data base, both dedicated.
        CallingConvention {
            // As `powerpc64_elf_v1`, with f1..f8.
            arg_passing_regs_float: &[
                "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "vs34", "vs35", "vs36", "vs37",
                "vs38", "vs39", "vs40", "vs41", "vs42", "vs43", "vs44", "vs45",
            ],
            // Above the back-chain and LR save words.
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            reg_args_adjoin_stack_args: false,
            // gcc returns `_Complex double` in r3..r6, not f1:f2. Its
            // `_Complex long double` in r3..r10 is left out: four more
            // `Return` inputs on every function for a rare type.
            ret_val_regs: &["r3", "r4", "r5", "r6"],
            ..Self::powerpc64_elf_v1()
        }
    }

    /// The PowerPC 64-bit ELFv1 calling convention. Conventions built with
    /// `..Self::powerpc64_elf_v1()` inherit every field they do not override.
    #[must_use]
    pub const fn powerpc64_elf_v1() -> Self {
        CallingConvention {
            stack_ptr: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            // f1..f13, then the vector arguments v2..v13 (`vs34`-`vs45`), appended
            // so the float positions do not shift.
            arg_passing_regs_float: &[
                "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
                "vs34", "vs35", "vs36", "vs37", "vs38", "vs39", "vs40", "vs41", "vs42", "vs43",
                "vs44", "vs45",
            ],
            callee_saved_regs: &[
                // r13 is the thread pointer, which no call moves; r2 (TOC) is listed
                // since a caller reloads it, keeping TOC-relative loads resolvable.
                "r2", "r13", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23",
                "r24", "r25", "r26", "r27", "r28", "r29", "r30", "r31",
                // LR is volatile, but listed so `InitialVar(LR)` survives calls and
                // a return through it is recognised.
                "LR", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24",
                "f25", "f26", "f27", "f28", "f29", "f30", "f31",
                // cr2-cr4 (not `crall`, which spans volatile fields), then v20-v31,
                // which the sla spells `vs52`-`vs63`.
                "cr2", "cr3", "cr4", "vs52", "vs53", "vs54", "vs55", "vs56", "vs57", "vs58", "vs59",
                "vs60", "vs61", "vs62", "vs63",
            ],
            ret_val_regs: &["r3", "r4"],
            // IBM double-double `long double` and `_Complex double` return in f1:f2,
            // `_Complex long double` in f1-f4. A vector returns in v2 (`vs34`), as
            // does an IEEE `__float128` and, under `-mabi=ieeelongdouble`, every
            // `long double`.
            ret_val_regs_float: &["f1", "f2", "f3", "f4", "vs34"],
            // ELFv1: the 48-byte linkage area plus the 64-byte parameter save area.
            stack_args: Some(StackArgs {
                base_offset: 112,
                increment: 8,
            }),
            ret_stack_pop: 0,
            link_register: Some("LR"),
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: true,
        }
    }

    /// The PowerPC 64-bit ELFv2 calling convention.
    #[must_use]
    pub const fn powerpc64_elf_v2() -> Self {
        CallingConvention {
            // The 32-byte linkage area plus the parameter save area. Ghidra's
            // cspec says 112, the ELFv1 figure.
            stack_args: Some(StackArgs {
                base_offset: 96,
                increment: 8,
            }),
            // Homogeneous float and vector aggregates of up to eight members.
            ret_val_regs_float: &[
                "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "vs34", "vs35", "vs36", "vs37",
                "vs38", "vs39", "vs40", "vs41",
            ],
            // The global entry receives its own address, from which the
            // prologue forms the TOC pointer.
            entry_address: Some("r12"),
            ..Self::powerpc64_elf_v1()
        }
    }

    /// The Linux kernel-internal CC for 32-bit PowerPC System V.
    #[must_use]
    pub const fn powerpc_sysv32_linux_kernel() -> Self {
        CallingConvention {
            arg_passing_regs_float: PPC32_SCALAR_ARGS_F,
            // Kernel code returns no `_Complex`.
            ret_val_regs: &["r3", "r4"],
            ret_val_regs_float: PPC_SCALAR_RET_F,
            ..Self::powerpc_sysv32()
        }
    }

    /// The Linux kernel-internal CC for PowerPC 64-bit ELFv1.
    #[must_use]
    pub const fn powerpc64_elf_v1_linux_kernel() -> Self {
        CallingConvention {
            arg_passing_regs_float: PPC64_SCALAR_ARGS_F,
            ret_val_regs_float: PPC_SCALAR_RET_F,
            callee_saved_regs: PPC64_KERNEL_SAVED,
            ..Self::powerpc64_elf_v1()
        }
    }

    /// The Linux kernel-internal CC for PowerPC 64-bit ELFv2.
    #[must_use]
    pub const fn powerpc64_elf_v2_linux_kernel() -> Self {
        CallingConvention {
            arg_passing_regs_float: PPC64_SCALAR_ARGS_F,
            ret_val_regs_float: &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"],
            callee_saved_regs: PPC64_KERNEL_SAVED,
            ..Self::powerpc64_elf_v2()
        }
    }
}
