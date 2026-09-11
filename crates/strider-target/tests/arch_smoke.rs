#![allow(clippy::type_complexity)]

//! Every [`strider_target::SleighArch`] preset must feed into
//! `rsleigh::Sleigh::new` and yield a usable register table.  Without this,
//! presets nothing else exercises (`mipsbe32`, `mipsle32`, `aarch64be`) would
//! silently rot when an upstream constant is renamed.

use strider_target::SleighArch;

fn assert_preset_resolves(label: &str, arch: SleighArch) {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .unwrap_or_else(|e| panic!("{label}: Sleigh::new failed: {e:?}"));
    sleigh
        .regs()
        .unwrap_or_else(|e| panic!("{label}: Sleigh::regs failed: {e:?}"));
}

#[test]
fn all_presets_resolve() {
    let cases: &[(&str, fn() -> SleighArch)] = &[
        ("x86_64", SleighArch::x86_64),
        ("x86", SleighArch::x86),
        ("mipsbe32", SleighArch::mipsbe32),
        ("mipsle32", SleighArch::mipsle32),
        ("arm", SleighArch::arm),
        ("aarch64", SleighArch::aarch64),
        ("aarch64be", SleighArch::aarch64be),
        ("mipsbe64", SleighArch::mipsbe64),
        ("mipsle64", SleighArch::mipsle64),
        ("ppc32be", SleighArch::ppc32be),
        ("ppc32le", SleighArch::ppc32le),
        ("ppc64be", SleighArch::ppc64be),
        ("ppc64le", SleighArch::ppc64le),
        ("arm_thumb", SleighArch::arm_thumb),
        ("arm_be", SleighArch::arm_be),
        ("arm_be_kernel", SleighArch::arm_be_kernel),
    ];
    for (label, ctor) in cases {
        assert_preset_resolves(label, ctor());
    }
}

/// The lifter's `vn_io` reads `register_endianness()` to pick the shift
/// direction when extracting a sub-register from its container, so a mistyped
/// value silently produces wrong shifts at the lift layer with no signal from
/// this crate.  `endianness()` is the DATA order and differs on BE8
/// (`arm_be_kernel`).  Pinned here so a typo in `arch.rs` fails at test time.
#[test]
fn presets_endianness_matches_arch() {
    use strider_target::Endianness;
    use strider_target::Endianness::{Big, Little};
    // (label, arch, data endianness, register endianness)
    let cases: &[(&str, SleighArch, Endianness, Endianness)] = &[
        ("x86_64", SleighArch::x86_64(), Little, Little),
        ("x86", SleighArch::x86(), Little, Little),
        ("mipsbe32", SleighArch::mipsbe32(), Big, Big),
        ("mipsle32", SleighArch::mipsle32(), Little, Little),
        ("mipsbe64", SleighArch::mipsbe64(), Big, Big),
        ("mipsle64", SleighArch::mipsle64(), Little, Little),
        ("arm", SleighArch::arm(), Little, Little),
        ("arm_thumb", SleighArch::arm_thumb(), Little, Little),
        ("arm_be", SleighArch::arm_be(), Big, Big),
        // BE8: big-endian data over a little-endian sla, the one preset where
        // the two orders differ.
        ("arm_be_kernel", SleighArch::arm_be_kernel(), Big, Little),
        ("aarch64", SleighArch::aarch64(), Little, Little),
        ("aarch64be", SleighArch::aarch64be(), Big, Big),
        ("ppc32be", SleighArch::ppc32be(), Big, Big),
        ("ppc32le", SleighArch::ppc32le(), Little, Little),
        ("ppc64be", SleighArch::ppc64be(), Big, Big),
        ("ppc64le", SleighArch::ppc64le(), Little, Little),
    ];
    for (label, arch, data, regs) in cases {
        assert_eq!(arch.endianness(), *data, "{label}: data order");
        assert_eq!(arch.register_endianness(), *regs, "{label}: register order");
    }
}

#[test]
fn arm_be_endianness_is_big() {
    use strider_target::Endianness;
    assert_eq!(SleighArch::arm_be().endianness(), Endianness::Big);
}

#[test]
fn arch_preset_variant_casing_compiles() {
    use strider_target::ArchPreset;
    let _v: &[ArchPreset] = &[
        ArchPreset::X86_64,
        ArchPreset::X86,
        ArchPreset::Arm,
        ArchPreset::ArmBe,
        ArchPreset::ArmThumb,
        ArchPreset::Aarch64,
        ArchPreset::Aarch64Be,
        ArchPreset::MipsBe32,
        ArchPreset::MipsLe32,
        ArchPreset::MipsBe64,
        ArchPreset::MipsLe64,
        ArchPreset::Ppc32Be,
        ArchPreset::Ppc32Le,
        ArchPreset::Ppc64Be,
        ArchPreset::Ppc64Le,
    ];
}
