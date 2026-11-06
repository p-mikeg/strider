use super::*;

/// Every family's exact rows.
fn family_rows() -> impl Iterator<Item = &'static Row> {
    crate::family::FAMILIES.iter().flat_map(|f| f.call_other)
}

/// The family rows answering `name` on `preset`.
fn family_rows_for(preset: crate::ArchPreset, name: &str) -> Vec<&'static Row> {
    preset
        .family()
        .call_other
        .iter()
        .filter(|row| row.matches(preset, name))
        .collect()
}

fn empty_abi() -> CallOtherAbi {
    CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: false,
    }
}

#[test]
fn set_isa_mode_is_noop_on_every_arch() {
    // `setISAMode` is NoOp on every arch (incl. ARM32); the mode is carried
    // on the `IndirectBranch`, not this op (see its row of `ROWS` in
    // `common.rs`).
    let arm32: Vec<_> = crate::ArchPreset::ALL
        .iter()
        .filter(|p| p.family().name == crate::family::ARM32.name)
        .collect();
    assert_eq!(arm32.len(), 4, "the ARM32 presets: {arm32:?}");
    for arm in arm32 {
        assert_eq!(
            classify(*arm, "setISAMode"),
            Some(CallOtherClass::NoOp),
            "{arm:?}"
        );
    }
    for other in [
        crate::ArchPreset::X86_64,
        crate::ArchPreset::Aarch64,
        crate::ArchPreset::MipsBe32,
        crate::ArchPreset::Ppc32Be,
    ] {
        assert_eq!(
            classify(other, "setISAMode"),
            Some(CallOtherClass::NoOp),
            "{other:?}"
        );
    }
}

/// The whole user-op set of the Thumb `msr control, Rn` constructor
/// (`ARMTHUMBinstructions.sinc`). `setThreadMode` is not one of them and
/// exists in no ARM sla.
#[test]
fn thumb_msr_control_user_ops_classify() {
    for n in [
        "isCurrentModePrivileged",
        "setThreadModePrivileged",
        "isThreadMode",
        "isUsingMainStack",
        "setStackMode",
    ] {
        assert!(classify(crate::ArchPreset::ArmThumb, n).is_some(), "{n}");
    }
    assert_eq!(classify(crate::ArchPreset::ArmThumb, "setThreadMode"), None);
}

#[test]
fn sysret_and_swapgs_are_x86_only() {
    for arch in [
        crate::ArchPreset::Arm,
        crate::ArchPreset::ArmBe,
        crate::ArchPreset::ArmThumb,
        crate::ArchPreset::Aarch64,
        crate::ArchPreset::Aarch64Be,
        crate::ArchPreset::MipsLe32,
        crate::ArchPreset::MipsBe32,
        crate::ArchPreset::MipsLe64,
        crate::ArchPreset::MipsBe64,
        crate::ArchPreset::Ppc32Le,
        crate::ArchPreset::Ppc32Be,
        crate::ArchPreset::Ppc64Le,
        crate::ArchPreset::Ppc64Be,
    ] {
        assert_eq!(classify(arch, "sysret"), None, "sysret on {arch:?}");
        assert_eq!(classify(arch, "swapgs"), None, "swapgs on {arch:?}");
    }
    // Still classified on x86 / x86_64.
    assert_eq!(
        classify(crate::ArchPreset::X86, "sysret"),
        Some(CallOtherClass::NO_RETURN)
    );
    assert_eq!(
        classify(crate::ArchPreset::X86_64, "sysret"),
        Some(CallOtherClass::NO_RETURN)
    );
}

#[test]
fn monitor_mwait_implicit_register_channels() {
    // Sleigh emits `monitor()` / `mwait()` with zero pcode operands, so
    // the register reads have to live in `implicit_reads`.  Per the Intel
    // SDM `MONITOR` and `MWAIT` entries.
    let m64 = classify(crate::ArchPreset::X86_64, "monitor").expect("monitor x86_64");
    let CallOtherClass::Call(abi) = m64 else {
        panic!("expected Call(abi) for monitor")
    };
    assert_eq!(abi.implicit_reads, &["RAX", "ECX", "EDX"]);
    assert!(abi.implicit_writes.is_empty());
    assert!(abi.clobbers_memory);

    let m32 = classify(crate::ArchPreset::X86, "monitor").expect("monitor x86");
    let CallOtherClass::Call(abi) = m32 else {
        panic!()
    };
    assert_eq!(abi.implicit_reads, &["EAX", "ECX", "EDX"]);

    let mwait = classify(crate::ArchPreset::X86_64, "mwait").expect("mwait classified");
    let CallOtherClass::Call(abi) = mwait else {
        panic!()
    };
    assert_eq!(abi.implicit_reads, &["EAX", "ECX"]);
    assert!(abi.implicit_writes.is_empty());
    assert!(abi.clobbers_memory);

    // AMD variants share the same shape.
    assert!(matches!(
        classify(crate::ArchPreset::X86_64, "monitorx"),
        Some(CallOtherClass::Call(_))
    ));
    assert!(matches!(
        classify(crate::ArchPreset::X86_64, "mwaitx"),
        Some(CallOtherClass::Call(_))
    ));

    // `monitor` is an English word and could appear in a future spec, so
    // the arch-specific guard has to keep it from matching elsewhere.
    assert_eq!(classify(crate::ArchPreset::Aarch64, "monitor"), None);
    assert_eq!(classify(crate::ArchPreset::Aarch64, "mwait"), None);
}

#[test]
fn ppc_call_others_classify_by_effect() {
    use crate::ArchPreset::{Ppc32Be, Ppc64Be};
    let mem =
        |n| matches!(classify(Ppc64Be, n), Some(CallOtherClass::Call(a)) if a.clobbers_memory);
    let pure = |n| matches!(classify(Ppc64Be, n), Some(CallOtherClass::Call(a)) if !a.clobbers_memory && !a.no_return);

    // Stores, atomics, and cache/TLB/SLB modification advance the memory
    // edge.
    for n in [
        "storeWordConditionalIndexed",
        "storeDoubleWordConditionalIndexed",
        "dataCacheBlockClearToZero",
        "LoadDoublewordByteReverseIndexed",
        "dataCacheBlockInvalidate",
        "TLBInvalidateEntry",
        "slbMoveToEntry",
    ] {
        assert!(mem(n), "{n} should clobber memory");
    }
    // Loads, SLB reads, the RNG, and every Altivec/VSX/vector compute op
    // are pure.
    for n in [
        "slbMoveFromEntryVSID",
        "random",
        "altv207_45",              // prefix
        "vsx300_19",               // prefix
        "vectorConditionalSelect", // prefix
    ] {
        assert!(pure(n), "{n} should be pure");
    }
    // The `vsx*` forms whose base-register access the sla leaves out of the
    // p-code must beat the prefix, or a load forwards across the store.
    for n in [
        "vsx300_15",
        "vsx300_20",
        "vsx300_22",
        "vsx207_1",
        "vsx300_9",
    ] {
        assert!(
            mem(n),
            "{n} names memory the p-code does not, so it must clobber"
        );
    }
    // `returnFromInterrupt` ends the function.
    assert_eq!(
        classify(Ppc64Be, "returnFromInterrupt"),
        Some(CallOtherClass::NO_RETURN)
    );
    // Conditional traps FALL THROUGH in Ghidra's model, so they must be
    // returning (memory-clobbering), never NO_RETURN, or code after a
    // compiler bounds/overflow trap is dropped from the IR.
    for n in ["trapWord", "trapDoubleWord", "trapDoubleWordImmediate"] {
        assert!(mem(n), "{n} must clobber memory");
        assert!(
            !classify(Ppc64Be, n).unwrap().is_no_return(),
            "{n} must return (fall-through)"
        );
    }
    // Prefetch hints are no-ops; `wait` parks the core so a remote agent
    // may touch memory, hence memory-clobbering.
    assert_eq!(
        classify(Ppc32Be, "dataCacheBlockTouch"),
        Some(CallOtherClass::NoOp)
    );
    assert!(mem("waitT"), "waitT must clobber memory");

    // The generic-word ops must not leak to another arch.
    for n in ["random", "message", "trapWord", "vectorConditionalSelect"] {
        assert_eq!(
            classify(crate::ArchPreset::Aarch64, n),
            None,
            "{n} leaked to AArch64"
        );
    }
}

#[test]
fn rdpkru_is_arch_specific_to_x86() {
    // rdpkru_u32 is an x86 family row because ECX/EAX/EDX only
    // resolve on x86 / x86_64.
    for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
        let class = classify(preset, "rdpkru_u32").expect("rdpkru classified");
        let CallOtherClass::Call(abi) = class else {
            panic!("expected Call")
        };
        assert_eq!(abi.implicit_reads, &["ECX"]);
        // EAX is the op's explicit pcode output, not an implicit write.
        assert_eq!(abi.implicit_writes, &["EDX"]);
        assert!(!abi.clobbers_memory);
    }
    assert_eq!(classify(crate::ArchPreset::Aarch64, "rdpkru_u32"), None);
    assert_eq!(classify(crate::ArchPreset::Arm, "rdpkru_u32"), None);
}

#[test]
fn msr_and_segment_base_ops_classify_pure_or_pure_with_mem_edge() {
    // Sleigh puts every register operand of these ops on the explicit
    // pcode chain, so the footprint is explicit.  The writes
    // carry a memory edge so opt passes cannot forward across them.
    let pure_ops = ["rdmsr", "readfsbase", "readgsbase"];
    let edge_ops = [
        ("wrmsr", &["FS_OFFSET", "GS_OFFSET"] as &[&str]),
        ("writefsbase", &["FS_OFFSET"]),
        ("writegsbase", &["GS_OFFSET"]),
        ("swapgs", &["GS_OFFSET"]),
    ];
    for preset in [crate::ArchPreset::X86, crate::ArchPreset::X86_64] {
        for n in pure_ops {
            let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{preset:?}/{n}: expected Call")
            };
            assert_eq!(abi, empty_abi(), "{preset:?}/{n}");
            assert!(!abi.clobbers_memory, "{preset:?}/{n}");
        }
        for (n, writes) in edge_ops {
            let class = classify(preset, n).unwrap_or_else(|| panic!("{preset:?}/{n}"));
            let CallOtherClass::Call(abi) = class else {
                panic!("{preset:?}/{n}: expected Call")
            };
            assert_eq!(abi.implicit_reads, &[] as &[&str], "{preset:?}/{n}");
            assert_eq!(abi.implicit_writes, writes, "{preset:?}/{n}");
            assert!(abi.clobbers_memory, "{preset:?}/{n}");
        }
    }
    // The encoded instructions only exist on x86/x86_64.
    for n in pure_ops.into_iter().chain(edge_ops.map(|(n, _)| n)) {
        assert_eq!(
            classify(crate::ArchPreset::Aarch64, n),
            None,
            "{n} on aarch64"
        );
        assert_eq!(classify(crate::ArchPreset::Arm, n), None, "{n} on arm");
    }
}

#[test]
fn syscall_is_arch_specific_to_x86_64() {
    // The arch-independent fallback must not supply "syscall" for
    // non-x86_64 presets, whose RAX/RDI names would not resolve.
    assert!(matches!(
        classify(crate::ArchPreset::X86_64, "syscall"),
        Some(CallOtherClass::Call(_)),
    ));
    assert_eq!(classify(crate::ArchPreset::X86, "syscall"), None);
    assert_eq!(classify(crate::ArchPreset::Aarch64, "syscall"), None);
    assert_eq!(classify(crate::ArchPreset::Arm, "syscall"), None);
}

/// The table's stated invariant, enforced over the table itself rather
/// than a hand-kept name list: a named register resolves only on the arch
/// whose Sleigh table holds it, so a row naming one builds on that arch
/// and errors on every other.
#[test]
fn arch_independent_call_entries_have_empty_register_channels() {
    for (name, class) in common::ROWS {
        let CallOtherClass::Call(abi) = class else {
            continue;
        };
        assert!(
            abi.implicit_reads.is_empty(),
            "arch-independent row {name:?} has non-empty implicit_reads \
             ({:?}); move it to its family's rows",
            abi.implicit_reads,
        );
        assert!(
            abi.implicit_writes.is_empty(),
            "arch-independent row {name:?} has non-empty implicit_writes \
             ({:?}); move it to its family's rows",
            abi.implicit_writes,
        );
    }
}

/// The family rows and the common rows must be name-disjoint per preset:
/// the lookup chain takes the first answer, so a name in both leaves the
/// later row dead, and every other table test passes, because each only
/// checks the answer that wins.
#[test]
fn no_name_is_classified_by_two_tables_on_one_preset() {
    let mut names: Vec<&str> = family_rows()
        .flat_map(|row| row.names.iter().copied())
        .collect();
    names.extend(common::ROWS.iter().map(|(n, _)| *n));
    names.sort_unstable();
    names.dedup();
    assert!(
        names.len() > 200,
        "the table scan found only {}",
        names.len()
    );

    let mut clashes = Vec::new();
    for preset in crate::ArchPreset::ALL {
        for name in &names {
            if !family_rows_for(*preset, name).is_empty()
                && common::ROWS.iter().any(|(n, _)| n == name)
            {
                clashes.push(format!(
                    "{preset:?}/{name}: the {} rows shadow common::ROWS",
                    preset.family().name
                ));
            }
        }
    }
    assert!(clashes.is_empty(), "{}", clashes.join("\n"));
}

/// The same first-answer-wins rule WITHIN one family's rows: `classify`
/// takes the first row that matches, so a second row matching one preset is
/// dead and the test above cannot see it. Several names sit in more than one
/// row, kept apart by `only` alone.
#[test]
fn no_arch_specific_row_shadows_another_on_one_preset() {
    let mut names: Vec<&str> = family_rows()
        .flat_map(|row| row.names.iter().copied())
        .collect();
    names.sort_unstable();
    names.dedup();
    let rows_holding = |name: &str| {
        family_rows()
            .filter(move |row| row.names.contains(&name))
            .count()
    };
    // Without a name in two rows nothing could ever clash, and widening
    // one row's `only` would leave this green.
    assert!(
        names.iter().filter(|n| rows_holding(n) > 1).count() >= 3,
        "premise: several names must sit in more than one row",
    );

    let mut clashes = Vec::new();
    for preset in crate::ArchPreset::ALL {
        for name in &names {
            let matching: Vec<&'static [&'static str]> = family_rows_for(*preset, name)
                .into_iter()
                .map(|row| row.names)
                .collect();
            if matching.len() > 1 {
                clashes.push(format!("{preset:?}/{name}: {matching:?}"));
            }
        }
    }
    assert!(clashes.is_empty(), "{}", clashes.join("\n"));
}

/// Arch-independence read off the table itself: every row answers the same
/// under every preset, save where an arch-specific row deliberately
/// shadows the name.
#[test]
fn arch_independent_entries_resolve_on_every_arch() {
    for (name, class) in common::ROWS {
        for preset in crate::ArchPreset::ALL {
            let Some(shadow) = family_rows_for(*preset, name).first().map(|row| row.class) else {
                assert_eq!(classify(*preset, name), Some(*class), "{preset:?}/{name}");
                continue;
            };
            assert_eq!(classify(*preset, name), Some(shadow), "{preset:?}/{name}");
        }
    }
}

/// The Vn-resolved override path: a caller with a footprint of its own
/// answers ahead of the tables, and names it did not claim still come
/// from them.
#[test]
fn an_override_carries_a_resolved_footprint() {
    let rcx = rsleigh::Vn {
        size: 8,
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let overrides = CallOtherOverrides::new(vec![(
        "rdtsc".to_string(),
        CallOtherOverride::Built(BuiltCallOtherAbi {
            implicit_reads: vec![rcx],
            implicit_writes: vec![],
            clobbers_memory: true,
            no_return: false,
        }),
    )])
    .expect("unique override names");
    match classify_with(&overrides, crate::ArchPreset::X86_64, "rdtsc") {
        Some(CallOtherLookup::Built(abi)) => {
            assert_eq!(abi.implicit_reads, vec![rcx]);
            assert!(abi.clobbers_memory);
        }
        other => panic!("expected a built override, got {other:?}"),
    }
    assert!(matches!(
        classify_with(&overrides, crate::ArchPreset::X86_64, "cpuid"),
        Some(CallOtherLookup::Class(_))
    ));
}

fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
    let arch = crate::SleighArch::x86_64();
    arch.probe_regs()
}

#[test]
fn build_syscall_x86_64_resolves_correct_vns() {
    let regs = x86_64_sleigh_regs();
    let abi = match classify(crate::ArchPreset::X86_64, "syscall").expect("syscall must classify") {
        CallOtherClass::Call(abi) => abi,
        other => panic!("expected Call(abi), got {other:?}"),
    };

    let built = abi.build(&regs).expect("syscall ABI must build on x86_64");

    let rax = regs.name_to_vn("RAX").expect("RAX must exist");
    assert!(
        built.implicit_reads.contains(&rax),
        "RAX must be in implicit_reads"
    );
    assert!(
        built.implicit_writes.contains(&rax),
        "RAX must be in implicit_writes"
    );
    assert!(built.clobbers_memory, "syscall must clobber memory");

    let expected_reads: Vec<rsleigh::Vn> = abi
        .implicit_reads
        .iter()
        .map(|n| {
            regs.name_to_vn(n)
                .unwrap_or_else(|| panic!("reg {n:?} not found"))
        })
        .collect();
    let expected_writes: Vec<rsleigh::Vn> = abi
        .implicit_writes
        .iter()
        .map(|n| {
            regs.name_to_vn(n)
                .unwrap_or_else(|| panic!("reg {n:?} not found"))
        })
        .collect();
    assert_eq!(
        built.implicit_reads, expected_reads,
        "implicit_reads mismatch"
    );
    assert_eq!(
        built.implicit_writes, expected_writes,
        "implicit_writes mismatch"
    );
    assert_eq!(built.clobbers_memory, abi.clobbers_memory);
}

#[test]
fn build_empty_channels_produces_empty_vecs() {
    let regs = x86_64_sleigh_regs();
    let abi = match classify(crate::ArchPreset::X86_64, "rdtsc").expect("rdtsc must classify") {
        CallOtherClass::Call(abi) => abi,
        other => panic!("expected Call(abi), got {other:?}"),
    };

    let built = abi.build(&regs).expect("rdtsc ABI must build");
    assert!(
        built.implicit_reads.is_empty(),
        "rdtsc has no implicit reads"
    );
    assert!(
        built.implicit_writes.is_empty(),
        "rdtsc has no implicit writes"
    );
    assert!(!built.clobbers_memory, "rdtsc does not clobber memory");
}

/// `build` must carry `no_return` onto the resolved ABI. Guards against
/// build silently dropping the flag, which would let a consumer of the
/// built form miss that a CallOther never returns.
#[test]
fn call_other_abi_build_preserves_no_return() {
    let regs = x86_64_sleigh_regs();
    let no_return_abi = CallOtherAbi {
        implicit_reads: &[],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: true,
    };
    assert!(
        no_return_abi.build(&regs).expect("must build").no_return,
        "build must preserve no_return = true"
    );
    let returning_abi = CallOtherAbi {
        no_return: false,
        ..no_return_abi
    };
    assert!(
        !returning_abi.build(&regs).expect("must build").no_return,
        "build must preserve no_return = false"
    );
}

#[test]
fn build_unknown_register_name_errors() {
    let regs = x86_64_sleigh_regs();
    let abi = CallOtherAbi {
        implicit_reads: &["NONEXISTENT_REG_XYZZY"],
        implicit_writes: &[],
        clobbers_memory: false,
        no_return: false,
    };
    let result = abi.build(&regs);
    assert!(result.is_err(), "unknown register must produce an error");
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("NONEXISTENT_REG_XYZZY"),
        "error must name the bad register; got: {msg}",
    );
}

fn mem_clobbers(c: Option<CallOtherClass>) -> bool {
    matches!(c, Some(CallOtherClass::Call(abi)) if abi.clobbers_memory)
}
fn is_pure(c: Option<CallOtherClass>) -> bool {
    matches!(c, Some(CallOtherClass::Call(abi)) if !abi.clobbers_memory && !abi.no_return)
}

#[test]
fn coproc_family_classifies_by_operation_not_direction() {
    use crate::ArchPreset::Arm;
    // The MRC/MCR direction does NOT indicate a side effect: cache / TLB
    // / barrier / WFI maintenance ops are named `coproc_movefrom_*` too and
    // must clobber memory, while a plain system-register move must not.
    for op in [
        "coproc_movefrom_Data_Memory_Barrier",
        "coproc_movefrom_Clean_Data_Cache_by_MVA",
        "coproc_movefrom_Invalidate_unified_TLB_unlocked",
        "coproc_movefrom_Data_Synchronization",
        "coproc_movefrom_Flush_Prefetch_Buffer",
        "coproc_movefrom_Wait_for_interrupt",
        "coproc_moveto_Clean_Entire_Data_Cache",
        "coproc_moveto_Invalidate_Entire_Instruction",
        // Writing MMU / translation-control registers changes the memory
        // view, so a load must not forward across the write.
        "coproc_moveto_Control",
        "coproc_moveto_Context_ID",
        "coproc_moveto_Translation_table_base_0",
        "coproc_moveto_Domain_Access_Control",
    ] {
        assert!(
            mem_clobbers(classify(Arm, op)),
            "{op} is a cache/TLB/barrier or MMU-control write, so it must MEM_CLOBBER",
        );
    }
    for op in [
        "coproc_movefrom_Main_ID",
        "coproc_movefrom_User_R_Thread_and_Process_ID",
        // Reading a control register is pure; only the write clobbers.
        "coproc_movefrom_Control",
        "coproc_movefrom_Translation_table_base_0",
    ] {
        assert!(
            is_pure(classify(Arm, op)),
            "{op} is a plain system-register read, so it must be PURE",
        );
    }
}

#[test]
fn prefix_families_are_arch_scoped() {
    use crate::ArchPreset::{Aarch64, Arm, X86_64};
    // `Vector*` and `Float*` are ARM-32's NEON spelling; AArch64 uses
    // `NEON_*` and declares neither.
    assert!(is_pure(classify(Arm, "VectorMultiply")));
    assert!(is_pure(classify(Arm, "FloatVectorMult")));
    assert_eq!(classify(Aarch64, "VectorMultiply"), None);
    assert_eq!(classify(Aarch64, "FloatVectorMult"), None);
    assert!(is_pure(classify(Aarch64, "NEON_fmul")));
    assert_eq!(classify(X86_64, "VectorMultiply"), None);
    // PowerPC and x86 spell unrelated ops with the `Float` stem, which the
    // arch scoping keeps out of the family.
    assert_eq!(classify(X86_64, "FloatingPointAdd"), None);
    // The AArch64 cache/TLB families have no 32-bit ARM or x86 user-op.
    assert!(mem_clobbers(classify(Aarch64, "TLBI_ALLE1")));
    assert!(mem_clobbers(classify(Aarch64, "DC_ZVA")));
    assert!(mem_clobbers(classify(Aarch64, "IC_IALLU")));
    assert_eq!(classify(Arm, "TLBI_ALLE1"), None);
    assert_eq!(classify(X86_64, "DC_ZVA"), None);
    assert_eq!(classify(X86_64, "coproc_movefrom_Main_ID"), None);
}

#[test]
fn generic_coprocessor_ops_split_read_vs_opaque() {
    use crate::ArchPreset::ArmThumb;
    // Reads are pure; opaque moveto/load/store/function clobber memory.
    assert!(is_pure(classify(ArmThumb, "coprocessor_movefromRt")));
    assert!(is_pure(classify(ArmThumb, "coprocessor_movefrom2")));
    for op in [
        "coprocessor_moveto",
        "coprocessor_moveto2",
        "coprocessor_load",
        "coprocessor_storelong",
        "coprocessor_function",
    ] {
        assert!(mem_clobbers(classify(ArmThumb, op)), "{op}");
    }
}

/// Every table row must name a `define pcodeop` the vendored sla actually
/// declares; a row naming nothing is unreachable weight. The prefix
/// families match by stem, not by row, so they are out of scope here.
#[test]
fn every_table_row_names_a_pcodeop_the_vendored_sla_declares() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../externals/rsleigh/sleigh/processors");
    let mut declared = std::collections::HashSet::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "sinc") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for tail in text.split("define pcodeop").skip(1) {
                    let name: String = tail
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    declared.insert(name);
                }
            }
        }
    }
    assert!(
        declared.len() > 1000,
        "only {} pcodeops found; is the rsleigh submodule checked out?",
        declared.len(),
    );

    let rows = common::ROWS
        .iter()
        .map(|(n, _)| *n)
        .chain(family_rows().flat_map(|r| r.names.iter().copied()));
    for name in rows {
        assert!(
            declared.contains(name),
            "call_other_abi row {name:?} names no `define pcodeop` in the vendored sla",
        );
    }
}
