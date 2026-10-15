//! PowerPC `linkreg` is the one committed context var in any shipped preset
//! that neither `SleighArch::flow_commit_vars` nor
//! `SleighArch::transient_decode_vars` nor `SleighArch::isa_mode_var` covers,
//! so a value one function commits does reach a later decode on a reused
//! engine. It is excluded for exactly one reason: every constructor a
//! `linkreg=1` selects emits p-code identical to its `linkreg=0` twin
//! (`ppc_instructions.sinc:311` against `:306`, and so on), so a stale value
//! cannot change the lift.
//!
//! That reason had no test. `commits_flow_context`'s doc cites this one.
//!
//! Asserts the property depended on, not the constructor selection: with
//! `linkreg=1` and `vle=0` both a twin and its `$(NOTVLE)` sibling match, and
//! because their bodies are identical nothing observable says which Sleigh
//! chose. That is also why it does not matter -- what the exclusion rests on
//! is that the lift is the same either way, which is exactly what fails here
//! if an sla update ever makes a pair diverge.

use strider_target::{ArchPreset, SleighArch};

const BASE: u64 = 0x1000;

const PPC_PRESETS: [ArchPreset; 4] = [
    ArchPreset::Ppc32Be,
    ArchPreset::Ppc32Le,
    ArchPreset::Ppc64Be,
    ArchPreset::Ppc64Le,
];

/// `b +8` (`OP=18`, `AA=0`, `LK=0`), the shape whose `linkreg=1` twin at
/// `ppc_instructions.sinc:310` drops the `$(NOTVLE)` guard and so is the one
/// a stale value could divert the decode to.
fn branch_bytes(arch: &SleighArch) -> Vec<u8> {
    let word: u32 = 0x4800_0008;
    let mut v = match arch.endianness() {
        strider_target::Endianness::Big => word.to_be_bytes().to_vec(),
        strider_target::Endianness::Little => word.to_le_bytes().to_vec(),
    };
    v.resize(0x20, 0);
    v
}

fn opcodes(lift: &rsleigh::LiftRes) -> Vec<String> {
    lift.insns
        .iter()
        .map(|i| format!("{:?}", i.opcode))
        .collect()
}

#[test]
fn a_stale_linkreg_cannot_change_a_powerpc_lift() {
    for preset in PPC_PRESETS {
        let arch = preset.arch();
        let reader = rsleigh::mem_readers::BufMemReader::new(branch_bytes(&arch), BASE);
        let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
            .unwrap_or_else(|e| panic!("{preset:?}: {e:?}"));

        assert_eq!(
            sleigh.get_context_at(BASE, "linkreg").expect("linkreg"),
            0,
            "{preset:?}: the pspec pins linkreg=0, which is the baseline here",
        );
        let at_zero = opcodes(&sleigh.lift_one(BASE).expect("lift at linkreg=0"));
        let len_zero = sleigh.lift_one(BASE).expect("re-lift").machine_insn_len;

        // The value `mtlr` commits, and ~45 constructors commit at their own
        // `inst_start`, left behind by an earlier function on this engine.
        sleigh
            .set_context_at(BASE, "linkreg", 1)
            .expect("commit linkreg=1");
        let at_one = opcodes(&sleigh.lift_one(BASE).expect("lift at linkreg=1"));
        let len_one = sleigh.lift_one(BASE).expect("re-lift").machine_insn_len;

        assert_eq!(
            (at_zero, len_zero),
            (at_one, len_one),
            "{preset:?}: a stale linkreg changed the lift, so excluding it from \
             every re-derivation list is unsound",
        );
    }
}
