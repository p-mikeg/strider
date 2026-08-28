//! Every [`strider_target::SleighArch`] preset must feed into
//! `rsleigh::Sleigh::new` and yield a usable register table.  Without this,
//! presets nothing else exercises (`mipsbe32`, `mipsle32`, `aarch64be`) would
//! silently rot when an upstream constant is renamed.

use strider_target::{ArchPreset, Endianness, SleighArch};

fn assert_preset_resolves(preset: ArchPreset, arch: SleighArch) {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .unwrap_or_else(|e| panic!("{preset:?}: Sleigh::new failed: {e:?}"));
    sleigh
        .regs()
        .unwrap_or_else(|e| panic!("{preset:?}: Sleigh::regs failed: {e:?}"));
}

#[test]
fn all_presets_resolve() {
    for preset in ArchPreset::ALL {
        assert_preset_resolves(*preset, preset.arch());
    }
}

/// The lifter's `vn_io` reads `register_endianness()` to pick the shift
/// direction when extracting a sub-register from its container, so a mistyped
/// value silently produces wrong shifts at the lift layer with no signal from
/// this crate.  `endianness()` is the DATA order and differs on BE8
/// (`arm_be_kernel`).  Pinned here so a typo in `arch.rs` fails at test time.
#[test]
fn presets_endianness_matches_arch() {
    use strider_target::Endianness::{Big, Little};
    // (preset, data endianness, register endianness)
    let cases: &[(ArchPreset, Endianness, Endianness)] = &[
        (ArchPreset::X86_64, Little, Little),
        (ArchPreset::X86, Little, Little),
        (ArchPreset::MipsBe32, Big, Big),
        (ArchPreset::MipsLe32, Little, Little),
        (ArchPreset::MipsBe64, Big, Big),
        (ArchPreset::MipsLe64, Little, Little),
        (ArchPreset::Arm, Little, Little),
        (ArchPreset::ArmThumb, Little, Little),
        (ArchPreset::ArmBe, Big, Big),
        // BE8: big-endian data over a little-endian sla, the one preset where
        // the two orders differ.
        (ArchPreset::ArmBeKernel, Big, Little),
        (ArchPreset::Aarch64, Little, Little),
        (ArchPreset::Aarch64Be, Big, Big),
        (ArchPreset::Ppc32Be, Big, Big),
        (ArchPreset::Ppc32Le, Little, Little),
        (ArchPreset::Ppc64Be, Big, Big),
        (ArchPreset::Ppc64Le, Little, Little),
    ];
    for preset in ArchPreset::ALL {
        assert!(
            cases.iter().any(|(p, ..)| p == preset),
            "{preset:?} has no pinned endianness pair"
        );
    }
    for (preset, data, regs) in cases {
        let arch = preset.arch();
        assert_eq!(arch.endianness(), *data, "{preset:?}: data order");
        assert_eq!(
            arch.register_endianness(),
            *regs,
            "{preset:?}: register order"
        );
    }
}
