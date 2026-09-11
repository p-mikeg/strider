//! Trap user-ops split by what the trap means to the OS, not by sla shape:
//! MIPS' `trap` carries Linux's `BUG()`, while ARM's `bkpt` does not.

use strider_target::ArchPreset;
use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass, classify};

fn class(preset: ArchPreset, name: &str) -> CallOtherClass {
    classify(preset, name).unwrap_or_else(|| panic!("{name} is unclassified on {preset:?}"))
}

fn abi(preset: ArchPreset, name: &str) -> CallOtherAbi {
    match class(preset, name) {
        CallOtherClass::Call(abi) => abi,
        other => panic!("{name}: expected Call(_), got {other:?}"),
    }
}

/// Linux MIPS spells `BUG()` as `break BRK_BUG` and `BUG_ON(c)` as
/// `tne $0,c,BRK_BUG`; both lift to `trap(tmp)` and `BUG()` is `__noreturn`.
/// `WARN_ON` does not reach `trap`: no `__bug_table` accompanies these traps,
/// and `__warn` / `warn_slowpath_fmt` are called instead.
#[test]
fn mips_trap_terminates() {
    for preset in [
        ArchPreset::MipsBe32,
        ArchPreset::MipsLe32,
        ArchPreset::MipsBe64,
        ArchPreset::MipsLe64,
    ] {
        assert!(
            class(preset, "trap").is_no_return(),
            "{preset:?}: BUG() does not return"
        );
    }
}

/// The conditional forms keep their non-trapping path through the sla's own
/// `if (!cond) goto <done>`, so terminating at the CallOther does not strand
/// the code after a `BUG_ON`. Guarding the arch-specific row: ARM's `bkpt` is
/// a debugger trap that resumes, and must NOT follow MIPS here.
#[test]
fn arm_bkpt_still_falls_through() {
    let bkpt = abi(ArchPreset::Arm, "software_bkpt");
    assert!(!bkpt.no_return, "`bkpt` resumes");
}

/// The `goto [target]` ops stay terminating. Classifying them returning does
/// NOT recover the WARN fall-through: it only converts the trailing branch
/// into an unresolved indirect branch. Recovering it needs the `goto` seated
/// at `inst_next`.
#[test]
fn goto_shaped_traps_terminate() {
    for (preset, name) in [
        (ArchPreset::Aarch64, "SoftwareBreakpoint"),
        (ArchPreset::X86_64, "invalidInstructionException"),
        (ArchPreset::Arm, "software_udf"),
    ] {
        assert!(
            class(preset, name).is_no_return(),
            "{name}: the sla branches to an unknown handler"
        );
    }
}

/// Writes that change what an address means: a load must not forward across them.
#[test]
fn arm_address_translation_writes_clobber_memory() {
    for name in [
        "coproc_moveto_FCSE_PID",
        "coproc_moveto_Peripherial_Port_Memory_Remap",
        "coproc_moveto_Peripheral_Port_Memory_Remap",
        "coproc_moveto_Secure_Configuration",
        "coproc_moveto_Security_world_control",
    ] {
        assert!(
            abi(ArchPreset::Arm, name).clobbers_memory,
            "{name} changes address translation, so it must clobber memory"
        );
    }
}
