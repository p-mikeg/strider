//! Smoke tests: every [`target::SleighArch`] preset must successfully feed
//! into `rsleigh::Sleigh::new` and resolve its documented stack pointer
//! register.  Without this, presets that nothing else exercises (e.g.
//! `mipsbe32`, `mipsle32`, `aarch64be`) could silently rot if an upstream
//! constant were renamed.

#![allow(clippy::panic, clippy::unwrap_used)]

use target::SleighArch;

fn assert_preset_resolves(label: &str, arch: SleighArch) {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap_or_else(|e| panic!("{label}: Sleigh::new failed: {e:?}"));
    let regs = sleigh
        .regs()
        .unwrap_or_else(|e| panic!("{label}: Sleigh::regs failed: {e:?}"));
    assert!(
        regs.name_to_vn(arch.stack_ptr_reg_name).is_some(),
        "{label}: stack_ptr_reg_name {:?} must resolve",
        arch.stack_ptr_reg_name,
    );
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
