//! PowerPC instructions whose result the sla leaves to a user op write their
//! destination from that op's output (Power ISA v3.0B): `slbmfee`, `slbmfev`
//! and `slbfee.` write RT (Book III chapter 5, SLB management), `vminud`
//! writes vrD (Book I chapter 6), `xsiexpdp` writes XT and the VSX exponent
//! compares and data-class tests write CR field BF (Book I chapter 7), and
//! `paste.` writes CR0 (Book II 4.4). Each value is architecturally defined
//! but unknown here, so it stays opaque.

mod common;

use common::Arch;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker};

const BASE: u64 = 0x1000;
const BLR: u32 = 0x4e80_0020;
/// `li r3, 1` / `li r3, 2`
const LI_R3_1: u32 = 0x3860_0001;
const LI_R3_2: u32 = 0x3860_0002;

fn analyze(arch: Arch, words: &[u32]) -> Function {
    let bytes = common::words_image(words, arch.sleigh().endianness());
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider
        .analyze(BASE, &cc, &Default::default(), None)
        .unwrap_or_else(|e| panic!("{arch:?}: {e}"));
    assert!(
        result.incomplete().is_empty(),
        "{arch:?}: {:?}",
        result.incomplete()
    );
    result.function
}

/// The `r3` each `Return` hands back, as constants, sorted.
fn returned_r3s(f: &Function) -> Vec<Option<u128>> {
    let mut r3s: Vec<_> = f
        .walk()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .map(|ret| f.int_const_u128(f.node_inputs(ret)[2]))
        .collect();
    r3s.sort_unstable();
    r3s
}

/// The `CallOther` named `name` that `v` is computed from through integer
/// slicing alone, if any.
fn sliced_from_call_other(
    f: &Function,
    v: ValueId,
    name: &str,
) -> Option<strider_ir::node::NodeId> {
    let producer = f.producer(v);
    match f.node_kind(producer) {
        NodeKind::CallOther { .. } => {
            (f.call_other_name(producer) == Some(name)).then_some(producer)
        }
        NodeKind::Truncate | NodeKind::Extend(_) | NodeKind::IntBinaryOp(_) => f
            .node_inputs(producer)
            .into_iter()
            .find_map(|i| sliced_from_call_other(f, i, name)),
        _ => None,
    }
}

/// `li r6, 0x123; slbmfe? r5, r6; mr r3, r5; blr`
#[test]
fn slbmfee_and_slbmfev_write_their_destination() {
    let mut wrong = Vec::new();
    for arch in [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le] {
        for (insn, word, op) in [
            ("slbmfee", 0x7ca0_3726, "slbMoveFromEntryESID"),
            ("slbmfev", 0x7ca0_36a6, "slbMoveFromEntryVSID"),
        ] {
            let f = analyze(arch, &[0x38c0_0123, word, 0x7ca3_2b78, BLR]);
            let r3 = common::returned(&f)[0];
            let producer = f.producer(r3);
            let ok = f.call_other_name(producer) == Some(op)
                && f.int_const_u128(f.node_inputs(producer)[2]) == Some(0x123);
            if !ok {
                wrong.push(format!(
                    "{arch:?} {insn}: r3 is {:?}",
                    f.node_kind(producer)
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `li r4, 0x11; li r5, 0x22; xsiexpdp vs0, r4, r5; mffprd r3, f0; blr`
#[test]
fn xsiexpdp_writes_its_target() {
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = analyze(
            arch,
            &[0x3880_0011, 0x38a0_0022, 0xf004_2f2c, 0x7c03_0066, BLR],
        );
        let r3 = common::returned(&f)[0];
        let op = sliced_from_call_other(&f, r3, "vsx300_49")
            .unwrap_or_else(|| panic!("{arch:?}: r3 is {:?}", f.kind_of_value(r3)));
        let args: Vec<_> = f
            .node_inputs(op)
            .into_iter()
            .skip(2)
            .map(|a| f.int_const_u128(a))
            .collect();
        assert_eq!(args, [Some(0x11), Some(0x22)], "{arch:?}");
    }
}

/// `li r4, 0; cmpwi r4, 0; cmpwi cr6, r4, 0; <compare>; beq crN, 1f;
/// li r3, 1; blr; 1: li r3, 2; blr`. Both `cmpwi` set EQ, so a `beq` on a
/// field the compare does not write always branches.
fn branch_on_a_compare(arch: Arch, compare: u32, beq_field: u32) -> Vec<Option<u128>> {
    let beq = 0x4182_000c | (beq_field << 18);
    let f = analyze(
        arch,
        &[
            0x3880_0000,
            0x2c04_0000,
            0x2f04_0000,
            compare,
            beq,
            LI_R3_1,
            BLR,
            LI_R3_2,
            BLR,
        ],
    );
    returned_r3s(&f)
}

/// Each compare, as `cr0` and as `cr6` (`BF << 23`), sets the field it names
/// and leaves the other.
#[test]
fn a_vsx_compare_writes_its_condition_register_field() {
    const BOTH: [Option<u128>; 2] = [Some(1), Some(2)];
    const TAKEN: [Option<u128>; 1] = [Some(2)];
    let mut wrong = Vec::new();
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        for (name, cr0_form) in [
            ("xscmpexpdp cr0, vs1, vs2", 0xf001_11d8_u32),
            ("xscmpexpqp cr0, v1, v2", 0xfc01_1148),
            ("xscmpoqp cr0, v1, v2", 0xfc01_1108),
            ("xscmpuqp cr0, v1, v2", 0xfc01_1508),
            ("xststdcdp cr0, vs1, 127", 0xf07f_0da8),
            ("xststdcqp cr0, v1, 127", 0xfc7f_0d88),
            ("xststdcsp cr0, vs1, 127", 0xf07f_0ca8),
        ] {
            let cr6_form = cr0_form | (6 << 23);
            for (bf, word, beq_field, expected) in [
                (0, cr0_form, 0, &BOTH[..]),
                (6, cr6_form, 6, &BOTH[..]),
                (6, cr6_form, 0, &TAKEN[..]),
                (0, cr0_form, 6, &TAKEN[..]),
            ] {
                let got = branch_on_a_compare(arch, word, beq_field);
                if got != expected {
                    wrong.push(format!(
                        "{arch:?} {name} as cr{bf}, beq cr{beq_field}: {got:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `paste. r3, r4; beq 1f; li r3, 1; blr; 1: li r3, 2; blr`: CR0 reports
/// whether the paste succeeded.
#[test]
fn paste_dot_sets_cr0_from_the_paste() {
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = analyze(
            arch,
            &[0x7c23_270d, 0x4182_000c, LI_R3_1, BLR, LI_R3_2, BLR],
        );
        assert_eq!(returned_r3s(&f), [Some(1), Some(2)], "{arch:?}");
    }
}

/// `li r4, 0x123; slbfee. r3, r4; blr`
#[test]
fn slbfee_dot_writes_its_destination() {
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = analyze(arch, &[0x3880_0123, 0x7c60_27a7, BLR]);
        let producer = f.producer(common::returned(&f)[0]);
        assert_eq!(
            f.call_other_name(producer),
            Some("slbfeeDotOp"),
            "{arch:?}: r3 is {:?}",
            f.node_kind(producer)
        );
        assert_eq!(
            f.int_const_u128(f.node_inputs(producer)[2]),
            Some(0x123),
            "{arch:?}"
        );
    }
}

/// `vminud v2, v3, v4; li r5, 0x2000; stvx v2, 0, r5; blr`
#[test]
fn vminud_writes_its_target() {
    for arch in [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le] {
        let f = analyze(arch, &[0x1043_22c2, 0x38a0_2000, 0x7c40_29ce, BLR]);
        let store = f
            .walk()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("one Store");
        let stored = f.node_inputs(store)[NodeKind::STORE_DATA_SLOT];
        assert!(
            sliced_from_call_other(&f, stored, "altv207_24").is_some(),
            "{arch:?}: v2 is {:?}",
            f.kind_of_value(stored)
        );
    }
}
