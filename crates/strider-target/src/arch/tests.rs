use super::{ArchPreset, SleighArch};

/// Naming vars to re-derive implies an ISA-mode var, which keeps
/// `Builder::is_benign_prefix_reentry`'s throwaway decode away from every
/// preset whose commits are believed: that probe is gated on
/// `isa_mode_var` being `None`, and `Builder::enqueue` reads the engine
/// only where `flow_commit_vars` is non-empty, so the two are disjoint.
#[test]
fn a_preset_that_re_derives_context_has_an_isa_mode_var() {
    for preset in ArchPreset::ALL {
        let arch = preset.arch();
        assert!(
            arch.flow_commit_vars().is_empty() || arch.isa_mode_var().is_some(),
            "{preset:?} names vars to re-derive but has no ISA-mode var, so a \
             throwaway decode could reach an address whose commits are believed"
        );
    }
}

/// `arm_thumb` is `arm-linux-gnueabihf-gcc -mthumb` over the full-ARM-state
/// `SLA_SPEC_ARM8_LE`, not a Cortex-M: its own fixture
/// (`fixtures/out/arm_thumb/arithmetic.elf`) carries `$a` ARM-state mapping
/// symbols and an even-addressed `FUNC call_weak_fn` at 0x530. Pinning
/// `TMode = 1` decodes those as garbage Thumb with no error, and withholding
/// the carry var leaves a resolved interworking branch unable to recover ARM.
#[test]
fn arm_thumb_honours_the_address_low_bit_and_carries_its_mode() {
    assert_eq!(
        SleighArch::arm_thumb().entry_mode_context(0x0530),
        Some(("TMode", 0)),
        "an even entry is ARM state, which this sla and preset both have"
    );
    assert_eq!(
        SleighArch::arm_thumb().entry_mode_context(0x05f9),
        Some(("TMode", 1)),
        "a Thumb function symbol carries the Thumb bit"
    );
    assert_eq!(SleighArch::arm_thumb().isa_mode_var(), Some("TMode"));
}

/// The family each preset belongs to, spelled out. Every other family
/// test reads the expected answer THROUGH `family()`, so a preset in the
/// wrong family moves both sides and survives; this one does not.
#[test]
fn every_preset_is_in_its_own_family() {
    use super::ArchPreset;
    let cases: &[(ArchPreset, &str)] = &[
        (ArchPreset::X86, "x86"),
        (ArchPreset::X86_64, "x86"),
        (ArchPreset::Arm, "arm32"),
        (ArchPreset::ArmBe, "arm32"),
        (ArchPreset::ArmBeKernel, "arm32"),
        (ArchPreset::ArmThumb, "arm32"),
        (ArchPreset::Aarch64, "aarch64"),
        (ArchPreset::Aarch64Be, "aarch64"),
        (ArchPreset::MipsBe32, "mips"),
        (ArchPreset::MipsLe32, "mips"),
        (ArchPreset::MipsBe64, "mips"),
        (ArchPreset::MipsLe64, "mips"),
        (ArchPreset::Ppc32Be, "ppc"),
        (ArchPreset::Ppc32Le, "ppc"),
        (ArchPreset::Ppc64Be, "ppc"),
        (ArchPreset::Ppc64Le, "ppc"),
    ];
    for preset in ArchPreset::ALL {
        assert!(
            cases.iter().any(|(p, _)| p == preset),
            "{preset:?} has no pinned family"
        );
    }
    for (preset, family) in cases {
        assert_eq!(preset.family_name(), *family, "{preset:?}");
    }
}

/// Each preset's Ghidra language id and its data and REGISTER-space byte
/// orders, spelled out: only BE8 `ArmBeKernel` has the two differ.
#[test]
fn every_preset_pins_its_ghidra_id_and_byte_orders() {
    use super::Endianness::{Big, Little};
    use super::{ArchPreset, Endianness};
    let cases: &[(ArchPreset, &str, Endianness, Endianness)] = &[
        (ArchPreset::X86, "x86:LE:32:default", Little, Little),
        (ArchPreset::X86_64, "x86:LE:64:default", Little, Little),
        (ArchPreset::Arm, "ARM:LE:32:v8", Little, Little),
        (ArchPreset::ArmBe, "ARM:BE:32:v8", Big, Big),
        (
            ArchPreset::ArmBeKernel,
            "ARM:LEBE:32:v8LEInstruction",
            Big,
            Little,
        ),
        (ArchPreset::ArmThumb, "ARM:LE:32:v8T", Little, Little),
        (ArchPreset::Aarch64, "AARCH64:LE:64:v8A", Little, Little),
        (ArchPreset::Aarch64Be, "AARCH64:BE:64:v8A", Big, Big),
        (ArchPreset::MipsBe32, "MIPS:BE:32:default", Big, Big),
        (ArchPreset::MipsLe32, "MIPS:LE:32:default", Little, Little),
        (ArchPreset::MipsBe64, "MIPS:BE:64:default", Big, Big),
        (ArchPreset::MipsLe64, "MIPS:LE:64:default", Little, Little),
        (ArchPreset::Ppc32Be, "PowerPC:BE:32:default", Big, Big),
        (ArchPreset::Ppc32Le, "PowerPC:LE:32:default", Little, Little),
        (ArchPreset::Ppc64Be, "PowerPC:BE:64:A2ALT", Big, Big),
        (ArchPreset::Ppc64Le, "PowerPC:LE:64:A2ALT", Little, Little),
    ];
    for preset in ArchPreset::ALL {
        assert!(
            cases.iter().any(|(p, ..)| p == preset),
            "{preset:?} has no pinned row"
        );
    }
    for (preset, id, data, register) in cases {
        let arch = preset.arch();
        assert_eq!(preset.ghidra_id(), *id, "{preset:?}");
        assert_eq!(arch.endianness(), *data, "{preset:?} data");
        assert_eq!(arch.register_endianness(), *register, "{preset:?} register");
    }
}

/// Each family's Ghidra ldefs file, spelled out.
#[test]
fn every_preset_pins_its_ghidra_ldefs() {
    use super::ArchPreset;
    let cases: &[(&str, &str)] = &[
        ("x86", "x86/data/languages/x86.ldefs"),
        ("arm32", "ARM/data/languages/ARM.ldefs"),
        ("aarch64", "AARCH64/data/languages/AARCH64.ldefs"),
        ("mips", "MIPS/data/languages/mips.ldefs"),
        ("ppc", "PowerPC/data/languages/ppc.ldefs"),
    ];
    for preset in ArchPreset::ALL {
        let Some((_, ldefs)) = cases.iter().find(|(f, _)| *f == preset.family_name()) else {
            panic!("{preset:?} has no pinned ldefs");
        };
        assert_eq!(preset.ghidra_ldefs(), *ldefs, "{preset:?}");
    }
}

/// Every preset carries the mode it decodes with, and only the ARM and
/// MIPS ones have a mode at all.
#[test]
fn isa_mode_var_matches_entry_mode_context_on_every_preset() {
    use super::ArchPreset;
    for preset in ArchPreset::ALL {
        let arch = preset.arch();
        assert_eq!(
            arch.isa_mode_var(),
            arch.entry_mode_context(0).map(|(v, _)| v),
            "{preset:?} must carry the mode it decodes with",
        );
        let expected = matches!(preset.family_name(), "arm32" | "mips");
        assert_eq!(
            arch.isa_mode_var().is_some(),
            expected,
            "{preset:?} ISA-mode presence",
        );
    }
}

/// The rule on [`super::SleighArch::callother_output_is_exact_in`], pinned
/// per preset and over the x87 exclusion within x86.
#[test]
fn callother_output_is_exact_only_where_the_machine_preserves_the_rest() {
    use super::ArchPreset;
    // RAX and ST0.
    let gpr = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: 0,
        size: 8,
    };
    let st0 = rsleigh::Vn { size: 10, ..gpr };
    for preset in ArchPreset::ALL {
        let expected = matches!(preset, ArchPreset::X86 | ArchPreset::X86_64);
        assert_eq!(
            preset.arch().callother_output_is_exact_in(&gpr),
            expected,
            "{preset:?}",
        );
        assert!(
            !preset.arch().callother_output_is_exact_in(&st0),
            "{preset:?} x87 container",
        );
    }
}

/// Which registers each preset reads as volatile: the counters are in, a
/// general-purpose register is out, and only AArch64 covers its whole
/// system-register file (`tpidr_el1` included).
#[test]
fn volatile_registers_per_preset() {
    for preset in ArchPreset::ALL {
        let arch = preset.arch();
        let regs = arch.probe_regs();
        let ranges = arch.volatile_register_ranges(&regs).expect("resolves");
        let volatile = |name: &str| {
            let vn = regs.name_to_vn(name).expect(name);
            let end = vn.addr_off + u64::from(vn.size);
            ranges.iter().any(|r| r.start < end && vn.addr_off < r.end)
        };
        let (counters, plain): (&[&str], &[&str]) = match preset.family_name() {
            "x86" | "arm32" => (&[], &[]),
            "aarch64" => (
                &[
                    "cntvct_el0",
                    "cntpct_el0",
                    "pmccntr_el0",
                    "tpidr_el1",
                    "tpidr_el2",
                    // Either side of the excluded `sp_el0`.
                    "elr_el1",
                    "spsel",
                ],
                &["x0", "sp", "NG", "sp_el0"],
            ),
            "mips" => (
                &["Count", "Random", "Cause", "PerfCnt.1", "cop0_reg9.6"],
                &["v0", "Status", "Compare", "PerfCnt"],
            ),
            "ppc" => (
                &[
                    "TBLr", "TBUr", "spr016", "spr313", "spr31a", "spr135", "spr351",
                ],
                &["r3", "LR", "spr110", "spr036", "spr15f"],
            ),
            other => panic!("{preset:?}: no volatile pins for family {other}"),
        };
        assert_eq!(ranges.is_empty(), counters.is_empty(), "{preset:?}");
        for name in counters {
            assert!(volatile(name), "{preset:?} {name}");
        }
        for name in plain {
            assert!(!volatile(name), "{preset:?} {name}");
        }
    }
}

/// Every constructor stamps its own roster row's preset, so a copy-paste in
/// the roster cannot make two presets indistinguishable.
#[test]
fn preset_round_trips_through_its_constructor() {
    for preset in super::ArchPreset::ALL {
        assert_eq!(preset.arch().preset(), *preset);
    }
}

#[test]
fn entry_mode_context_maps_low_bit_per_arch() {
    // MIPS: the low bit selects the alternate ISA via ISA_MODE.
    assert_eq!(
        SleighArch::mipsbe32().entry_mode_context(0x0040_0a90),
        Some(("ISA_MODE", 0))
    );
    assert_eq!(
        SleighArch::mipsle64().entry_mode_context(0x0040_0a91),
        Some(("ISA_MODE", 1))
    );
    // ARM: the low bit is the Thumb bit via TMode.
    assert_eq!(
        SleighArch::arm().entry_mode_context(0x1001),
        Some(("TMode", 1))
    );
    assert_eq!(
        SleighArch::arm_thumb().entry_mode_context(0x1001),
        Some(("TMode", 1))
    );
    assert_eq!(
        SleighArch::arm_thumb().entry_mode_context(0x1000),
        Some(("TMode", 0))
    );
    // Arches with no address-encoded entry mode.
    assert_eq!(SleighArch::x86_64().entry_mode_context(0x1000), None);
    assert_eq!(SleighArch::ppc32be().entry_mode_context(0x1001), None);
}
