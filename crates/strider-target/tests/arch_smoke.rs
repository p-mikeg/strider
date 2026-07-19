#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::type_complexity
)]

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
    ];
    for (label, ctor) in cases {
        assert_preset_resolves(label, ctor());
    }
}

/// `strider::register_aliasing` reads `Endianness` to pick the shift
/// direction when extracting a sub-register from its container, so a mistyped
/// value silently produces wrong shifts at the analyzer layer with no signal
/// from this crate.  Pinned here so a typo in `arch.rs` fails at test time.
#[test]
fn presets_endianness_matches_arch() {
    use strider_target::Endianness;
    let cases: &[(&str, SleighArch, Endianness)] = &[
        ("x86_64", SleighArch::x86_64(), Endianness::Little),
        ("x86", SleighArch::x86(), Endianness::Little),
        ("mipsbe32", SleighArch::mipsbe32(), Endianness::Big),
        ("mipsle32", SleighArch::mipsle32(), Endianness::Little),
        ("mipsbe64", SleighArch::mipsbe64(), Endianness::Big),
        ("mipsle64", SleighArch::mipsle64(), Endianness::Little),
        ("arm", SleighArch::arm(), Endianness::Little),
        ("arm_thumb", SleighArch::arm_thumb(), Endianness::Little),
        ("aarch64", SleighArch::aarch64(), Endianness::Little),
        ("aarch64be", SleighArch::aarch64be(), Endianness::Big),
        ("ppc32be", SleighArch::ppc32be(), Endianness::Big),
        ("ppc32le", SleighArch::ppc32le(), Endianness::Little),
        ("ppc64be", SleighArch::ppc64be(), Endianness::Big),
        ("ppc64le", SleighArch::ppc64le(), Endianness::Little),
    ];
    for (label, arch, expected) in cases {
        assert_eq!(
            arch.endianness(),
            *expected,
            "{label}: expected {expected:?}, got {:?}",
            arch.endianness(),
        );
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
