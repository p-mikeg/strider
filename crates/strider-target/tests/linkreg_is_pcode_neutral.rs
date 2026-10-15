//! PowerPC `linkreg` is the one committed context var in any shipped preset
//! that neither `SleighArch::flow_commit_vars` nor
//! `SleighArch::transient_decode_vars` nor `SleighArch::isa_mode_var` covers,
//! so a value one function commits does reach a later decode on a reused
//! engine. It is excluded for exactly one reason: the lift is the same either
//! way. `SleighArch::commits_flow_context`'s doc cites this file for it.
//!
//! Six constructors are guarded on `linkreg=1`, each with a twin that is not.
//! Whether Sleigh actually SELECTS the twin is observable, because every twin
//! carries `[ linkreg=0; globalset(inst_start,linkreg) ]` and so drives the
//! value at its own address to 0: only `bctr` and `mtlr` specialize their
//! sibling strictly enough to win. `TwinAt::Runs` records that, and is
//! asserted, because a test over an encoding whose twin is unreachable would
//! lift one constructor twice and compare it with itself.

use strider_target::{ArchPreset, Endianness, SleighArch};

const BASE: u64 = 0x1000;

const PPC_PRESETS: [ArchPreset; 4] = [
    ArchPreset::Ppc32Be,
    ArchPreset::Ppc32Le,
    ArchPreset::Ppc64Be,
    ArchPreset::Ppc64Le,
];

/// What a `linkreg=1` lift proves about which constructor matched.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TwinAt {
    /// The twin wins, and drives `linkreg` to 0 at its own address.
    Runs,
    /// The sibling wins whatever `linkreg` holds, so the twin is unreachable
    /// and the value survives.
    Unreachable,
    /// Both constructors commit `linkreg=0` here, so selection leaves no
    /// trace. The p-code comparison still applies.
    NotObservable,
}

/// `(word, what it is, the twin's site, selection)`.
const TWINS: [(u32, &str, &str, TwinAt); 6] = [
    (
        0x4800_0008,
        "b +8",
        "ppc_instructions.sinc:311 vs :306",
        TwinAt::Unreachable,
    ),
    (
        0x4280_0000,
        "b (OP=16)",
        "ppc_instructions.sinc:350 vs :345",
        TwinAt::Unreachable,
    ),
    (
        0x4080_0000,
        "b^CC",
        "ppc_instructions.sinc:386 vs :382",
        TwinAt::NotObservable,
    ),
    (
        0x4e80_0420,
        "bctr",
        "ppc_instructions.sinc:466 vs :461",
        TwinAt::Runs,
    ),
    (
        0x4e80_0460,
        "bctar",
        "ppc_isa.sinc:2237 vs :2232",
        TwinAt::Unreachable,
    ),
    (
        0x7c08_03a6,
        "mtlr",
        "ppc_embedded.sinc:160 vs :155",
        TwinAt::Runs,
    ),
];

fn image(arch: &SleighArch, word: u32) -> Vec<u8> {
    let mut v = match arch.endianness() {
        // PowerPC little-endian byte-swaps the instruction word
        // (`ppc_*_le.slaspec` sets `ENDIAN "little"`).
        Endianness::Big => word.to_be_bytes().to_vec(),
        Endianness::Little => word.to_le_bytes().to_vec(),
    };
    v.resize(0x20, 0);
    v
}

/// A FRESH engine per context value: a twin's own commit would otherwise make
/// the second measurement a `linkreg=0` one.
fn lift_at(arch: &SleighArch, word: u32, linkreg: u32) -> Option<(String, usize, u32)> {
    let reader = rsleigh::mem_readers::BufMemReader::new(image(arch, word), BASE);
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    sleigh
        .set_context_at(BASE, "linkreg", linkreg)
        .expect("linkreg is a PowerPC context var");
    let lift = sleigh.lift_one(BASE).ok()?;
    // Whole `Insn`s, not just opcodes: `bctr` and `bctar` are both a bare
    // `BranchIndirect` and differ only in the register they read, so an
    // opcode-only comparison cannot see an operand diverging.
    Some((
        format!("{:?}", lift.insns),
        lift.machine_insn_len,
        sleigh.get_context_at(BASE, "linkreg").expect("read back"),
    ))
}

#[test]
fn a_stale_linkreg_cannot_change_a_powerpc_lift() {
    let mut selected = 0usize;
    for preset in PPC_PRESETS {
        let arch = preset.arch();
        for (word, what, site, twin) in TWINS {
            let Some(at_zero) = lift_at(&arch, word, 0) else {
                // `bctar` is Power ISA 2.07 and does not decode on the 32-bit
                // presets. Nothing to compare, and nothing to excuse.
                assert!(
                    lift_at(&arch, word, 1).is_none(),
                    "{preset:?}/{what}: decodes at linkreg=1 but not at 0",
                );
                continue;
            };
            let at_one = lift_at(&arch, word, 1).expect("decodes at linkreg=0, so also at 1");

            assert_eq!(
                (&at_zero.0, at_zero.1),
                (&at_one.0, at_one.1),
                "{preset:?}/{what}: a stale linkreg changed the lift, so \
                 excluding it from every re-derivation list is unsound ({site})",
            );

            match twin {
                TwinAt::Runs => {
                    assert_eq!(
                        at_one.2, 0,
                        "{preset:?}/{what}: the twin no longer wins, so this row \
                         now compares one constructor with itself ({site})",
                    );
                    selected += 1;
                }
                TwinAt::Unreachable => assert_eq!(
                    at_one.2, 1,
                    "{preset:?}/{what}: the twin became reachable, so its \
                     p-code is now load-bearing here ({site})",
                ),
                TwinAt::NotObservable => {}
            }
        }
    }
    assert!(
        selected >= PPC_PRESETS.len(),
        "no preset exercised a twin that actually wins, so nothing here \
         compared two different constructors",
    );
}
