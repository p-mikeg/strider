//! Smoke tests: every [`target::SleighArch`] preset must successfully feed
//! into `rsleigh::Sleigh::new` and produce a usable register table.  Without
//! this, presets that nothing else exercises (e.g. `mipsbe32`, `mipsle32`,
//! `aarch64be`) could silently rot if an upstream constant were renamed.
//!
//! Stack-pointer resolution is covered by the `calling_convention` tests in
//! the crate's unit-test module — this file intentionally does not assert it,
//! because the SP name lives on `CallingConvention`, not `SleighArch`.

#![allow(clippy::panic, clippy::unwrap_used)]

use target::SleighArch;

fn assert_preset_resolves(label: &str, arch: SleighArch) {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap_or_else(|e| panic!("{label}: Sleigh::new failed: {e:?}"));
    sleigh
        .regs()
        .unwrap_or_else(|e| panic!("{label}: Sleigh::regs failed: {e:?}"));
}

#[test]
fn x86_64_preset_resolves() {
    assert_preset_resolves("x86_64", SleighArch::x86_64());
}

#[test]
fn x86_preset_resolves() {
    assert_preset_resolves("x86", SleighArch::x86());
}

#[test]
fn mipsbe32_preset_resolves() {
    assert_preset_resolves("mipsbe32", SleighArch::mipsbe32());
}

#[test]
fn mipsle32_preset_resolves() {
    assert_preset_resolves("mipsle32", SleighArch::mipsle32());
}

#[test]
fn arm_preset_resolves() {
    assert_preset_resolves("arm", SleighArch::arm());
}

#[test]
fn aarch64_preset_resolves() {
    assert_preset_resolves("aarch64", SleighArch::aarch64());
}

#[test]
fn aarch64be_preset_resolves() {
    assert_preset_resolves("aarch64be", SleighArch::aarch64be());
}

/// Pins the [`target::Endianness`] field of every `SleighArch` preset.
///
/// `Endianness` is consumed by `analyzer::register_aliasing` to decide the
/// shift direction when extracting a sub-register from its container; a
/// mistyped value on a BE preset (or vice-versa) silently produces wrong
/// shifts at the analyzer layer, with no signal from this crate.  Pin it
/// here so a typo in `arch.rs` is caught at unit-test time.
#[test]
fn presets_endianness_matches_arch() {
    use target::Endianness;
    let cases: &[(&str, SleighArch, Endianness)] = &[
        ("x86_64", SleighArch::x86_64(), Endianness::Little),
        ("x86", SleighArch::x86(), Endianness::Little),
        ("mipsbe32", SleighArch::mipsbe32(), Endianness::Big),
        ("mipsle32", SleighArch::mipsle32(), Endianness::Little),
        ("arm", SleighArch::arm(), Endianness::Little),
        ("aarch64", SleighArch::aarch64(), Endianness::Little),
        ("aarch64be", SleighArch::aarch64be(), Endianness::Big),
    ];
    for (label, arch, expected) in cases {
        assert_eq!(
            arch.endianness, *expected,
            "{label}: expected {expected:?}, got {:?}",
            arch.endianness,
        );
    }
}
