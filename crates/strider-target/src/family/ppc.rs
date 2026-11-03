use super::{DecodeFacts, Family};
use crate::ArchPreset;
use crate::call_other_abi::{CallOtherAbi, CallOtherClass, MEM_CLOBBER, PURE, Prefix, Row};
use crate::calling_convention::{CallingConvention, CcPreset, StackArgs};
use CallOtherClass::NoOp;

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
const PPC_FPCC: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["fp_cc0", "fp_cc1", "fp_cc2", "fp_cc3"],
    clobbers_memory: false,
    no_return: false,
});

/// [`PPC_FPCC`] plus the invalid-operation bits and summaries a
/// quad-precision compare raises (VXSNAN, and VXVC for `xscmpoqp`).
const PPC_FPCC_VX: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &[
        "fp_cc0",
        "fp_cc1",
        "fp_cc2",
        "fp_cc3",
        "fp_fx",
        "fp_fex",
        "fp_vx",
        "fp_vxsnan",
        "fp_vxvc",
    ],
    clobbers_memory: false,
    no_return: false,
});

/// The FPSCR status bits a scalar VSX arithmetic, rounding or conversion op
/// may set: FPRF (`fp_c`, FPCC), FR, FI, the exceptions and their summaries.
///
/// The scalar float ops the sla spells in p-code (`fadd`, `fmadd`, `frsp`,
/// `fctiw`, `xsadddp`, ...) set their bits in p-code instead: FR, FI, OX and
/// UX from the `float*` rows' ops, the rest from their operands.
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
const PPC_FP_INVALID: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["fp_fx", "fp_fex", "fp_vx", "fp_vxsnan", "fp_vxvc"],
    clobbers_memory: false,
    no_return: false,
});

/// [`PPC_FP_INVALID`] plus the CR6 an `Rc=1` vector compare sets.
const PPC_FP_INVALID_CR6: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
    implicit_reads: &[],
    implicit_writes: &["fp_fx", "fp_fex", "fp_vx", "fp_vxsnan", "fp_vxvc", "cr6"],
    clobbers_memory: false,
    no_return: false,
});

/// Every FPSCR bit `mtfsf` can rewrite (`FM=0xff`), which `setSummaryFPSCR`
/// and `mcrfs` read back directly.
const PPC_FPSCR: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
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
        "fp_vxvc",
        "fp_fr",
        "fp_fi",
        "fp_c",
        "fp_cc0",
        "fp_cc1",
        "fp_cc2",
        "fp_cc3",
        "fp_reserve1",
        "fp_vxsoft",
        "fp_vxsqrt",
        "fp_vxcvi",
        "fp_ve",
        "fp_oe",
        "fp_ue",
        "fp_ze",
        "fp_xe",
        "fp_ni",
        "fp_rn0",
        "fp_rn1",
    ],
    clobbers_memory: false,
    no_return: false,
});

/// `mtfsf.` / `mtfsfi.`: the FPSCR write plus the CR1 the `Rc=1` form sets.
const PPC_FPSCR_CR1: CallOtherClass = CallOtherClass::Call(CallOtherAbi {
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
        "fp_vxvc",
        "fp_fr",
        "fp_fi",
        "fp_c",
        "fp_cc0",
        "fp_cc1",
        "fp_cc2",
        "fp_cc3",
        "fp_reserve1",
        "fp_vxsoft",
        "fp_vxsqrt",
        "fp_vxcvi",
        "fp_ve",
        "fp_oe",
        "fp_ue",
        "fp_ze",
        "fp_xe",
        "fp_ni",
        "fp_rn0",
        "fp_rn1",
        "cr1",
    ],
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

/// Linux PowerPC `sc`: r0 = syscall number, r3..r8 = args, return in r3.
///
/// Sleigh's `syscall()` has no pcode output, so this is the whole footprint.
/// The kernel ABI makes r0, r4-r12 and cr0 volatile, so naming only r3 would
/// forward a stale r9-r12 across the call. `XER` names only the SPR the
/// `mfspr` path reads; the bits `adde`/`addc`/`subfe` actually read and write
/// are the separate 1-byte registers at `offset=0x400`, so naming `XER` alone
/// leaves a carry live across the call.
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

/// GHIDRA's PowerPC `.sinc` lifts atomic store-conditional, cache / TLB / SLB
/// management, Altivec/VSX vector ops, traps, and system-register moves to
/// named `pcodeop` CallOthers.  Scoped to the four PPC presets so the generic
/// names among them (`random`, `message`) cannot match on another arch.
const CALL_OTHER: &[Row] = &[
    // `rfi`/`rfid` return from an exception handler and end the function.
    Row::new(&["returnFromInterrupt"], CallOtherClass::NO_RETURN),
    // `tw`/`td` conditional traps: GHIDRA emits the pcodeop unconditionally
    // with the trap-on condition as a plain operand (NO branch), so control
    // FALLS THROUGH. They must be RETURNING, not NO_RETURN, or every
    // function with a compiler bounds/overflow trap loses its tail. A
    // firing trap may enter a memory-touching handler, so MEM_CLOBBER.
    Row::new(&["trapDoubleWord"], MEM_CLOBBER),
    Row::new(&["trapDoubleWordImmediate"], MEM_CLOBBER),
    Row::new(&["trapWord"], MEM_CLOBBER),
    // The `vsx*` prefix below classifies VSX as pure register compute, which
    // holds for every form whose memory access the sla spells as a p-code Load
    // or Store. These nineteen are the exceptions: `vsx.sinc` emits a bare
    // pcodeop with the base register as an operand and no `*[ram]` anywhere, so
    // the access is invisible unless the row names it. Left pure, a `stxvl`
    // does not advance the memory token and a later load of the same address
    // forwards past it.
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
    // The arithmetic, rounding and conversion forms set FPSCR status bits
    // (Power ISA 3.0B, Book I chapter 7), which `mcrfs` and `mffs` read back.
    // The permutes, moves, sign and exponent ops and `xscvspdpn` set none.
    // `xscmpnedp` and `xvcmpne{dp,sp}[.]` are draft encodings no final ISA
    // holds and only the sla decodes; their footprints are `xscmpeqdp`'s and
    // `xvcmpeqdp[.]`'s.
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
    // Store-conditional and byte-reverse stores write RAM; dcbz zeroes a
    // block; dcbf/dcbst/icbi and TLB/SLB management are ordering barriers;
    // icswx/copy-paste and doorbell messages have external memory effects.
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
    // `wait` is a low-power wait like ARM WFI: a remote agent may modify
    // shared memory while the core is parked, so it must not let a load
    // forward across it.
    Row::new(&["waitT"], MEM_CLOBBER),
    // `ldbrx` is `D = LoadDoublewordByteReverseIndexed(D,A,B)`: base registers
    // by value, the 8-byte read inside the op, so the access is invisible on
    // the memory chain, the same shape as the `vsx*` loads above. `stdbrx` is
    // NOT that shape (`*[ram]:8 EA = StoreDoublewordByteReverseIndexed(...)`,
    // an explicit p-code store, leaving its op a pure byte swap); its clobber
    // above is an over-approximation.
    Row::new(&["LoadDoublewordByteReverseIndexed"], MEM_CLOBBER),
    // The vector shift-control generator (lvsl, `altivec.sinc:219`) and the
    // hardware RNG produce a pcode-explicit output and touch no RAM. Altivec/VSX/vector
    // compute is covered by the `altv`/`vsx`/`vector` prefix below.
    Row::new(&["loadVectorForShiftLeft"], PURE),
    Row::new(&["random"], PURE),
    // e500 `mfpmr` / `mtpmr` move a performance-monitor register
    // (`quicciii.sinc`) and touch no RAM. A `CallOther` is never
    // deduplicated, so two reads of a running counter stay two values.
    Row::new(&["moveFromPerformanceMonitorRegister"], PURE),
    Row::new(&["moveToPerformanceMonitorRegister"], PURE),
    // The scalar float ops the sla spells in p-code take FR, FI, OX and UX,
    // which hang on a rounding it does not compute, from these: each yields
    // the bit, which the sla writes (FR, FI) or ORs in (OX, UX). `frsp`
    // spells its own as `floatAdd*` of zero.
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
    // `slbmfee` / `slbmfev` / `slbfee.` write RT from the op's output (a fork
    // patch: the stock sla wrote it nowhere) and read the SLB, not RAM.
    Row::new(&["slbMoveFromEntryESID"], PURE),
    Row::new(&["slbMoveFromEntryVSID"], PURE),
    Row::new(&["slbfeeDotOp"], PPC_DOT_CR0),
    // Cache-touch and stream-stop are prefetch hints, and `clearHistory`
    // clears a branch-history buffer only a pcodeop with its own output reads.
    // No memory or value effect to model.
    //
    // The rows below are NOT inert: each userop is a bare void pcodeop, yet
    // the sla models the FPSCR bits (`ppc_common.sinc:43`) and MSR (`:52`) as
    // real registers it reads back itself, in `setSummaryFPSCR` (`:1736`) and
    // `mfmsr` (`ppc_embedded.sinc:93`). Undeclared, a read either side of one
    // folds to the same value.
    //
    // Narrow reach, unlike `wrtee` / `wrteei`: `mtfsf` ITSELF is modelled in
    // p-code (`ppc_instructions.sinc:2218`, which pins `BIT_25=0 & BIT_16=0`
    // and emits `packFPSCR` / `unpackFPSCR`), so these `MoveToFPSCRField*`
    // pcodeops exist only in `ppc_isa.sinc`, included only by the
    // `ppc_64_isa*` slaspecs. They therefore fire on the Power ISA 2.06
    // `L=1` / `W=1` forms on the ppc64 presets, and never on ppc32.
    Row::new(&["MoveToFPSCRFieldImmediate"], PPC_FPSCR),
    Row::new(&["MoveToFPSCRFieldImmediate1"], PPC_FPSCR_CR1),
    Row::new(&["MoveToFPSCRFields"], PPC_FPSCR),
    Row::new(&["MoveToFPSCRFields1"], PPC_FPSCR_CR1),
    Row::new(&["WriteExternalEnable"], PPC_MSR),
    Row::new(&["WriteExternalEnableImmediate"], PPC_MSR),
    Row::new(&["clearHistory"], NoOp),
    Row::new(&["dataCacheBlockTouch"], NoOp),
    Row::new(&["dataCacheBlockTouchForStore"], NoOp),
    Row::new(&["dataStreamStopAll"], NoOp),
];

/// GHIDRA emits Altivec / VSX / named vector intrinsics as `altv207_<n>`,
/// `vsx<ver>_<n>`, and `vector<Op>`.  Reached only after `CALL_OTHER`, which
/// holds the nineteen `vsx*` forms whose memory access the sla leaves out of
/// the p-code and the forms that also set FPSCR bits; everything else in
/// the three families is register compute touching no RAM, its destination
/// written from the op's output. The stock sla wrote some nowhere, and the
/// fork patches them: `xsiexpdp`'s XT, `vminud`'s vrD, and CR field BF of
/// the seven `PPC_FPCC` / `PPC_FPCC_VX` compares.
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

/// The float windows without the AltiVec window the userland presets append
/// to them, for the `*_linux_kernel` flavours: a PowerPC Linux kernel is built
/// `-mno-altivec -mno-vsx`, so no vector register carries an argument or a
/// result there.  Naming them anyway would seed twelve 16-byte varnodes on
/// every function and renumber `arg(n)`, neither of which the kernel's own
/// code can produce.
const PPC32_SCALAR_ARGS_F: &[&str] = &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"];
const PPC64_SCALAR_ARGS_F: &[&str] = &[
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13",
];
const PPC_SCALAR_RET_F: &[&str] = &["f1", "f2"];

/// Base for `powerpc64_elf_v1` and `powerpc64_elf_v2`, which differ in
/// `stack_args.base_offset` (ELFv1's 48-byte linkage area vs ELFv2's 32, each
/// plus the 64-byte parameter save area) and in `ret_val_regs_float`.
const POWERPC64_ELF_BASE: CallingConvention = CallingConvention {
    stack_ptr: "r1",
    arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
    // ELFv1 3.2.3 / ELFv2 2.2.3.1: floating-point arguments in f1..f13.  The
    // f1..f8 window is the 32-bit SysV rule (`powerpc_sysv32`).
    //
    // ELFv2 2.2.3 / the ELFv1 AltiVec supplement then pass vector arguments in
    // v2..v13, a register file of its own (`vs34`-`vs45`; see
    // `callee_saved_regs` for the `vN` spelling), byte-disjoint from f0-f31.
    // Appended AFTER the floats so the float positions do not shift: a vector
    // argument's own ordinal is not its index here, the same positional
    // approximation `arm_aapcs` documents.  `*_linux_kernel` drops the window
    // (`PPC64_SCALAR_ARGS_F`).
    arg_passing_regs_float: &[
        "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "f13", "vs34",
        "vs35", "vs36", "vs37", "vs38", "vs39", "vs40", "vs41", "vs42", "vs43", "vs44", "vs45",
    ],
    callee_saved_regs: &[
        // r13 is the userland TLS thread pointer, reserved and never written
        // there.  In the ppc64 Linux kernel it is the per-CPU `paca` pointer,
        // which `setup_paca`, `setjmp` / `longjmp`, `restore_gprs` and every
        // `mfsprg r13,1` interrupt entry write, and a task migrating inside a
        // callee resumes on another CPU's paca.  Listed callee-saved anyway:
        // the equality is then an
        // address-identity claim the kernel does not make, while the call's
        // memory clobber already cuts every load through it.
        //
        // r2 (TOC) is NOT callee-saved: ELFv1 3.5.11 / ELFv2 2.3.2.1 put the
        // save and restore on the CALLER, and a cross-module callee does alter
        // it.  Modelled preserved anyway, the same tradeoff as `LR` below: it
        // keeps TOC-relative loads resolvable across a call, and a caller that
        // needs r2 afterwards reloads it from the save slot, so the reload is
        // what a correct binary reads.
        "r2", "r13", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24",
        "r25", "r26", "r27", "r28", "r29", "r30", "r31",
        // PPC64 ELFv1 3.4 marks LR volatile/caller-saved.  Listing it
        // callee-saved anyway is the deliberate link-register tradeoff (same
        // as `powerpc_sysv32`): it makes `InitialVar(lr)` propagate through
        // call sites, so the indirect-branch resolver's `LinkRegister` arm
        // fires for functions returning via the entry LR.
        //
        // ELFv1 3.2.2 / ELFv2 2.2.1 make f14-f31 non-volatile.
        "LR", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25",
        "f26", "f27", "f28", "f29", "f30", "f31",
        // CR2-CR4 are the non-volatile condition fields; CR0/CR1 and CR5-CR7
        // are volatile. Named individually because that is how `mfcr` /
        // `mtcrf` name them, never as the 8-byte `crall` container, which
        // spans the volatile fields too.
        "cr2", "cr3", "cr4",
        // ELFv2 2.2.1 and the ELFv1 AltiVec supplement make v20-v31
        // non-volatile in full.  The sla overlays AltiVec onto the top 32 VSX
        // registers and leaves the `vN` spellings commented out, so v20-v31
        // are `vs52`-`vs63` (`ppc_common.sinc`).
        "vs52", "vs53", "vs54", "vs55", "vs56", "vs57", "vs58", "vs59", "vs60", "vs61", "vs62",
        "vs63",
    ],
    ret_val_regs: &["r3", "r4"],
    // `long double` is IBM double-double and returns in the f1:f2 pair, as
    // does `_Complex double`.  A vector returns in v2 (`vs34`), as does an
    // IEEE `__float128` / `_Float128` and, under `-mabi=ieeelongdouble`,
    // every `long double`.
    ret_val_regs_float: &["f1", "f2", "vs34"],
    // 48-byte linkage area, then the 8-doubleword parameter save area that
    // homes r3-r10; the first stack-ONLY argument is above both.  GHIDRA's
    // ppc_64_be.cspec agrees on 112.
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
};

impl CallingConvention {
    /// Returns the PowerPC 32-bit System V ABI calling convention.
    #[must_use]
    pub const fn powerpc_sysv32() -> Self {
        // PowerPC 32-bit System V, byte-order independent (`powerpc-linux-gnu-gcc`
        // with either `-mbig-endian` or `-mlittle-endian`).  `r1` is the SP by
        // PowerPC convention.  Stack args start at 8: a 4-byte back-chain plus a
        // 4-byte LR save.  r3/r4 double as the pair for 64-bit returns.
        CallingConvention {
            stack_ptr: "r1",
            arg_passing_regs: &["r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10"],
            // SysV PPC32: floating-point arguments in f1..f8, then the AltiVec
            // supplement's vector arguments in v2..v13 (`vs34`-`vs45`), the
            // same append-after-the-floats layout as `POWERPC64_ELF_BASE`.
            arg_passing_regs_float: &[
                "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "vs34", "vs35", "vs36", "vs37",
                "vs38", "vs39", "vs40", "vs41", "vs42", "vs43", "vs44", "vs45",
            ],
            callee_saved_regs: &[
                // r2 (system/TLS thread pointer) and r13 (small-data-area base)
                // are reserved/dedicated: preserved across calls like r14-r31.
                "r2", "r13", "r14", "r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23",
                "r24", "r25", "r26", "r27", "r28", "r29", "r30", "r31", "LR",
                // SysV PPC32 3-16: f14-f31 are non-volatile.
                "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25",
                "f26", "f27", "f28", "f29", "f30", "f31",
                // CR2-CR4 are the non-volatile condition fields; CR0/CR1 and
                // CR5-CR7 are volatile. Named individually because that is how
                // `mfcr` / `mtcrf` name them, never as the 8-byte `crall`
                // container, which spans the volatile fields too.
                "cr2", "cr3", "cr4",
                // The SysV AltiVec supplement makes v20-v31 non-volatile in
                // full; the sla spells them `vs52`-`vs63` (see
                // `POWERPC64_ELF_BASE`).
                "vs52", "vs53", "vs54", "vs55", "vs56", "vs57", "vs58", "vs59", "vs60", "vs61",
                "vs62", "vs63",
            ],
            ret_val_regs: &["r3", "r4"],
            // The f1:f2 pair carries `long double` / `_Complex double`; v2
            // carries a vector result (AltiVec supplement).
            ret_val_regs_float: &["f1", "f2", "vs34"],
            stack_args: Some(StackArgs {
                base_offset: 8,
                increment: 4,
            }),
            ret_stack_pop: 0,
            // `bl` writes the return address to the `LR` SPR; Sleigh's PPC
            // table names it uppercase.
            link_register: Some("LR"),
            preserves_memory: false,
            preserves_all_registers: false,
            entry_address: None,
            reg_args_adjoin_stack_args: false,
        }
    }

    /// Returns the PowerPC 64-bit ELFv1 calling convention.
    #[must_use]
    pub const fn powerpc64_elf_v1() -> Self {
        // PowerPC 64-bit ELFv1, BE (`powerpc64-linux-gnu-gcc`).  Stack args start
        // at 112: the 48-byte linkage area plus the 64-byte parameter save area.
        //
        // ELFv1 has function descriptors: an external function symbol resolves to
        // a 3-pointer descriptor (entry, TOC, env), not the entry itself.  Only
        // the register-level ABI is modelled here.
        POWERPC64_ELF_BASE
    }

    /// Returns the PowerPC 64-bit ELFv2 calling convention.
    #[must_use]
    pub const fn powerpc64_elf_v2() -> Self {
        // PowerPC 64-bit ELFv2, LE (`powerpc64le-linux-gnu-gcc`).  Drops function
        // descriptors, so symbols point straight at the entry point, and shrinks
        // the linkage area from 48 to 32 bytes.  Register usage matches ELFv1.
        CallingConvention {
            // 32-byte linkage area plus the same 8-doubleword parameter save
            // area, so arguments past the eighth start at r1+96.  GHIDRA's
            // ppc_64_le.cspec says 112, which is the ELFv1 figure copied over.
            stack_args: Some(StackArgs {
                base_offset: 96,
                increment: 8,
            }),
            // ELFv2 2.2.3.3 returns a homogeneous float aggregate of up to
            // eight members in f1-f8 and a homogeneous VECTOR aggregate of up
            // to eight in v2-v9 (`vs34`-`vs41`), so the whole window carries a
            // result and not just its first member.  ELFv1 returns either
            // aggregate through a hidden
            // pointer, so the base's f1:f2 pair (`long double`,
            // `_Complex double`) plus v2 is the whole story there.
            ret_val_regs_float: &[
                "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "vs34", "vs35", "vs36", "vs37",
                "vs38", "vs39", "vs40", "vs41",
            ],
            // ELFv2 2.2.2.1: a caller entering through the GLOBAL entry leaves
            // the function's own address in r12, which the two-instruction
            // prologue turns into the TOC pointer. ELFv1 has no such register
            // contract, hence the override here rather than in the base.
            entry_address: Some("r12"),
            ..POWERPC64_ELF_BASE
        }
    }

    /// Returns the Linux kernel-internal CC for 32-bit PowerPC System V.
    #[must_use]
    pub const fn powerpc_sysv32_linux_kernel() -> Self {
        CallingConvention {
            arg_passing_regs_float: PPC32_SCALAR_ARGS_F,
            ret_val_regs_float: PPC_SCALAR_RET_F,
            ..Self::powerpc_sysv32()
        }
    }

    /// Returns the Linux kernel-internal CC for PowerPC 64-bit ELFv1.
    #[must_use]
    pub const fn powerpc64_elf_v1_linux_kernel() -> Self {
        CallingConvention {
            arg_passing_regs_float: PPC64_SCALAR_ARGS_F,
            ret_val_regs_float: PPC_SCALAR_RET_F,
            ..POWERPC64_ELF_BASE
        }
    }

    /// Returns the Linux kernel-internal CC for PowerPC 64-bit ELFv2.
    #[must_use]
    pub const fn powerpc64_elf_v2_linux_kernel() -> Self {
        CallingConvention {
            arg_passing_regs_float: PPC64_SCALAR_ARGS_F,
            // ELFv2 2.2.3.3's homogeneous float aggregate, without the
            // homogeneous VECTOR aggregate the userland preset also returns.
            ret_val_regs_float: &["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"],
            ..Self::powerpc64_elf_v2()
        }
    }
}
