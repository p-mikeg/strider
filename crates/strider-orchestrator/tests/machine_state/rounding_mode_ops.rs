//! The rsleigh patches append each family's rounding-mode user ops after the
//! sla's own (x86's `xgetbv` follows them), so no other user op id moved.

use crate::common::{self, ALL_ARCHES, Arch};

/// The tail of [`strider_orchestrator::Strider::user_op_names`] per arch.
fn tail(arch: Arch) -> &'static [&'static str] {
    match arch {
        Arch::X86 | Arch::X86Kernel | Arch::X64 => &["x87RoundingControl", "xgetbv"],
        Arch::Aarch64 | Arch::Aarch64Be => &["fpcrRoundingControl"],
        Arch::Arm | Arch::ArmBe | Arch::ArmBe8 | Arch::ArmThumb => &["fpscrRoundingControl"],
        Arch::Mips32le | Arch::Mips32be | Arch::Mips64le | Arch::Mips64be => {
            &["fcsrRoundingControl"]
        }
        Arch::Ppc32be | Arch::Ppc32le | Arch::Ppc64be | Arch::Ppc64le => {
            &["fpscrRoundingControl", "fpscrUndefined"]
        }
    }
}

#[test]
fn the_rounding_mode_ops_are_the_last_user_ops() {
    for &arch in ALL_ARCHES {
        let (strider, _) = common::strider_over_bytes(arch, vec![0; 4], 0x1000, None);
        let names = strider.user_op_names();
        let tail = tail(arch);
        assert_eq!(names[names.len() - tail.len()..], *tail, "{arch:?}");
    }
}
