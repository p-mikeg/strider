//! The calling-convention roster against the named constructors and the arch
//! presets it describes.

use std::collections::BTreeSet;

use strider_target::{ArchPreset, CallingConvention, CcPreset};

fn named() -> [(&'static str, CallingConvention); 14] {
    [
        ("x86_cdecl", CallingConvention::x86_cdecl()),
        ("x86_linux_kernel", CallingConvention::x86_linux_kernel()),
        ("x86_64_systemv", CallingConvention::x86_64_systemv()),
        ("arm_aapcs", CallingConvention::arm_aapcs()),
        ("arm_aapcs_soft", CallingConvention::arm_aapcs_soft()),
        ("aarch64_aapcs64", CallingConvention::aarch64_aapcs64()),
        ("mips_o32", CallingConvention::mips_o32()),
        ("mips_n64", CallingConvention::mips_n64()),
        ("powerpc_sysv32", CallingConvention::powerpc_sysv32()),
        (
            "powerpc_sysv32_linux_kernel",
            CallingConvention::powerpc_sysv32_linux_kernel(),
        ),
        ("powerpc64_elf_v1", CallingConvention::powerpc64_elf_v1()),
        (
            "powerpc64_elf_v1_linux_kernel",
            CallingConvention::powerpc64_elf_v1_linux_kernel(),
        ),
        ("powerpc64_elf_v2", CallingConvention::powerpc64_elf_v2()),
        (
            "powerpc64_elf_v2_linux_kernel",
            CallingConvention::powerpc64_elf_v2_linux_kernel(),
        ),
    ]
}

#[test]
fn preset_names_are_unique_and_are_the_constructors() {
    let names: Vec<&str> = CallingConvention::presets().map(|p| p.name).collect();
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate preset name in {names:?}"
    );
    let expected: BTreeSet<&str> = named().iter().map(|(n, _)| *n).collect();
    assert_eq!(unique, expected);
}

#[test]
fn every_preset_is_its_named_constructor() {
    for (name, cc) in named() {
        let preset = CallingConvention::presets()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("no preset {name}"));
        assert_eq!(preset.cc, cc, "{name}");
    }
}

#[test]
fn every_preset_builds_on_every_arch_it_names() {
    for preset in CallingConvention::presets() {
        assert!(!preset.arches.is_empty(), "{} names no arch", preset.name);
        for arch in preset.arches {
            let names = arch.arch().names();
            let built = preset
                .cc
                .build(names.regs())
                .unwrap_or_else(|e| panic!("{} on {}: {e:#}", preset.name, arch.name()));
            built
                .validate()
                .unwrap_or_else(|e| panic!("{} on {}: {e:#}", preset.name, arch.name()));
            built
                .check_addr_size(names.ram_addr_size())
                .unwrap_or_else(|e| panic!("{} on {}: {e:#}", preset.name, arch.name()));
        }
    }
}

#[test]
fn every_arch_has_a_calling_convention() {
    for preset in ArchPreset::ALL {
        let ccs: Vec<&CcPreset> = preset.arch().calling_conventions().collect();
        assert!(
            !ccs.is_empty(),
            "{} has no calling convention",
            preset.name()
        );
        for cc in ccs {
            assert!(
                cc.arches.contains(preset),
                "{} on {}",
                cc.name,
                preset.name()
            );
        }
    }
}

#[test]
fn every_ghidra_entry_is_for_arches_the_preset_names() {
    for preset in CallingConvention::presets() {
        for (cspec, _, arches) in preset.ghidra {
            for arch in *arches {
                assert!(
                    preset.arches.contains(arch),
                    "{}: {cspec} pairs {} outside its arches",
                    preset.name,
                    arch.name()
                );
            }
        }
    }
}
