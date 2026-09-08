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

/// Every name `transient_decode_vars` returns must be a context var the
/// preset's own sla declares: the lifter reads each through `get_context_at`
/// and silently drops the ones that fail, so a misspelling leaves the var
/// leaking across functions with no signal.
#[test]
fn transient_decode_vars_resolve_on_their_preset() {
    for preset in ArchPreset::ALL {
        let arch = preset.arch();
        let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .unwrap_or_else(|e| panic!("{preset:?}: Sleigh::new failed: {e:?}"));
        for name in arch.transient_decode_vars() {
            sleigh
                .get_context_at(0, name)
                .unwrap_or_else(|e| panic!("{preset:?}/{name}: {e:?}"));
        }
    }
}

/// MIPS `PAIR_INSTRUCTION_FLAG` is `noflow` (`mips.sinc`) yet selects the
/// `lwl`/`swl`/`ldl`/`sdl` constructor that performs the whole unaligned
/// access, and `globalset(inst_next, ...)` paints it forward. Outside
/// `FlowVars` by construction, so only this list makes a cold entry clear it.
#[test]
fn mips_presets_clear_the_pair_instruction_flag() {
    for preset in [
        ArchPreset::MipsBe32,
        ArchPreset::MipsLe32,
        ArchPreset::MipsBe64,
        ArchPreset::MipsLe64,
    ] {
        assert_eq!(
            preset.arch().transient_decode_vars(),
            &["PAIR_INSTRUCTION_FLAG"],
            "{preset:?}"
        );
    }
    // ARM's three are unrelated and must not have picked it up.
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
