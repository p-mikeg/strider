//! Per-preset [`strider_target::SleighArch`] facts checked against every
//! preset, so ones nothing else exercises (`mipsbe32`, `mipsle32`,
//! `aarch64be`) do not silently rot when an upstream constant is renamed.

use strider_target::{ArchPreset, Endianness};

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

/// The two MIPS `noflow` context vars a `globalset(inst_next, ...)` paints
/// forward: `PAIR_INSTRUCTION_FLAG` (`mips.sinc:412`), which selects the
/// `lwl`/`swl`/`ldl`/`sdl` constructor performing the whole unaligned access,
/// and `ext_delay` (`mips.sinc:450`), which MIPS16e PC-relative address
/// arithmetic subtracts (`mips16.sinc:164`). Outside `FlowVars` by
/// construction, so only this list makes a cold entry clear them.
#[test]
fn mips_presets_clear_both_forward_painted_noflow_vars() {
    for preset in [
        ArchPreset::MipsBe32,
        ArchPreset::MipsLe32,
        ArchPreset::MipsBe64,
        ArchPreset::MipsLe64,
    ] {
        assert_eq!(
            preset.arch().transient_decode_vars(),
            &["PAIR_INSTRUCTION_FLAG", "ext_delay"],
            "{preset:?}"
        );
    }
    // ARM's LRset is unrelated and must not have picked it up.
    for preset in [ArchPreset::Arm, ArchPreset::ArmThumb] {
        let vars = preset.arch().transient_decode_vars();
        assert!(!vars.contains(&"PAIR_INSTRUCTION_FLAG"), "{preset:?}");
        assert!(vars.contains(&"LRset"), "{preset:?}");
    }
    assert!(
        ArchPreset::X86_64.arch().transient_decode_vars().is_empty(),
        "x86_64 declares none",
    );
}
