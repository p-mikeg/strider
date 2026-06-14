#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::type_complexity
)]

//! Smoke tests: every [`strider_target::SleighArch`] preset must successfully feed
//! into `rsleigh::Sleigh::new` and produce a usable register table.  Without
//! this, presets that nothing else exercises (e.g. `mipsbe32`, `mipsle32`,
//! `aarch64be`) could silently rot if an upstream constant were renamed.
//!
//! Stack-pointer resolution is covered by the `calling_convention` tests in
//! the crate's unit-test module — this file intentionally does not assert it,
//! because the SP name lives on `CallingConvention`, not `SleighArch`.

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

/// Pins the [`strider_target::Endianness`] field of every `SleighArch` preset.
///
/// `Endianness` is consumed by `strider::register_aliasing` to decide the
/// shift direction when extracting a sub-register from its container; a
/// mistyped value on a BE preset (or vice-versa) silently produces wrong
/// shifts at the analyzer layer, with no signal from this crate.  Pin it
/// here so a typo in `arch.rs` is caught at unit-test time.
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
