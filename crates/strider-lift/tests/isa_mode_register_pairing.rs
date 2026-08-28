//! The ISA-mode capture/carry pairing, over every arch rather than only the
//! ones a lift test happens to exercise.
//!
//! `SleighRegs::name_to_vn` reports an unknown register as `None`, not as an
//! error, so a misspelled `"ISAModeSwitch"` satisfies a one-directional check
//! vacuously while the mode bit is never captured. `FunctionLifter::new`
//! asserts the biconditional; this pins it against all 16 real sla register
//! tables, in release as well as debug.

use strider_target::ArchPreset;

/// The sla defines `ISAModeSwitch` exactly where the arch exposes an
/// `isa_mode_var` to carry it.
#[test]
fn isa_mode_switch_resolves_exactly_where_the_arch_carries_a_mode() {
    for &preset in ArchPreset::ALL {
        let arch = preset.arch();
        let regs = arch
            .probe_regs()
            .unwrap_or_else(|e| panic!("{preset:?}: probe_regs failed: {e}"));
        assert_eq!(
            regs.name_to_vn("ISAModeSwitch").is_some(),
            arch.isa_mode_var().is_some(),
            "{preset:?}: ISAModeSwitch resolved={}, isa_mode_var={:?}",
            regs.name_to_vn("ISAModeSwitch").is_some(),
            arch.isa_mode_var(),
        );
    }
}
