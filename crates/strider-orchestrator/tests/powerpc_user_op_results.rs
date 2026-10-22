//! PowerPC instructions whose result the sla leaves to a user op write their
//! destination from that op's output (Power ISA v3.0B): `slbmfee`, `slbmfev`
//! and `slbfee.` write RT (Book III chapter 5, SLB management), `vminud`
//! writes vrD (Book I chapter 6), `xsiexpdp` writes XT and the VSX exponent
//! compares and data-class tests write CR field BF (Book I chapter 7), and
//! `paste.` writes CR0 (Book II 4.4). Each value is architecturally defined
//! but unknown here, so it stays opaque.

mod common;

use common::Arch;
use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeId, NodeKind, ValueId};
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

/// The incoming value of the register `name`, if the function reads it.
fn initial(arch: Arch, f: &Function, name: &str) -> Option<ValueId> {
    let vn = arch
        .sleigh()
        .probe_regs()
        .name_to_vn(name)
        .unwrap_or_else(|| panic!("no register named {name:?}"));
    f.initial_var_value(&vn)
}

/// The `CallOther` named `name` that `v` is a slice of (a truncation,
/// extension, shift or constant mask of it), if any.
fn sliced_from_call_other(f: &Function, v: ValueId, name: &str) -> Option<NodeId> {
    let producer = f.producer(v);
    let inputs = f.node_inputs(producer);
    let sliced = match f.node_kind(producer) {
        NodeKind::CallOther { .. } => {
            return (f.call_other_name(producer) == Some(name)).then_some(producer);
        }
        NodeKind::Truncate
        | NodeKind::Extend(_)
        | NodeKind::IntBinaryOp(
            IntBinaryOp::ShiftRight | IntBinaryOp::SShiftRight | IntBinaryOp::ShiftLeft,
        ) => inputs[0],
        NodeKind::IntBinaryOp(IntBinaryOp::And) => match (
            f.int_const_u128(inputs[0]).is_some(),
            f.int_const_u128(inputs[1]).is_some(),
        ) {
            (true, false) => inputs[1],
            (false, true) => inputs[0],
            _ => return None,
        },
        _ => return None,
    };
    sliced_from_call_other(f, sliced, name)
}

/// The `CallOther` named `name`, which the function holds once.
fn the_call_other(f: &Function, name: &str) -> NodeId {
    let mut ops = f.walk().filter(|&n| f.call_other_name(n) == Some(name));
    let op = ops.next().unwrap_or_else(|| panic!("no {name}"));
    assert!(ops.next().is_none(), "two {name}");
    op
}

/// The operands of a `CallOther`, past its control and memory inputs.
fn operands(f: &Function, op: NodeId) -> Vec<ValueId> {
    f.node_inputs(op).into_iter().skip(2).collect()
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

/// A user-op operand: an incoming register or an immediate.
#[derive(Clone, Copy, Debug)]
enum Operand {
    Reg(&'static str),
    Imm(u128),
}

/// The seven compares as `BF = cr0`, their user op and its operands.
const COMPARES: [(&str, u32, &str, [Operand; 2]); 7] = {
    use Operand::{Imm, Reg};
    [
        (
            "xscmpexpdp cr0, vs1, vs2",
            0xf001_11d8,
            "vsx300_29",
            [Reg("vs1"), Reg("vs2")],
        ),
        (
            "xscmpexpqp cr0, v1, v2",
            0xfc01_1148,
            "vsx300_30",
            [Reg("vs33"), Reg("vs34")],
        ),
        (
            "xscmpoqp cr0, v1, v2",
            0xfc01_1108,
            "vsx300_34",
            [Reg("vs33"), Reg("vs34")],
        ),
        (
            "xscmpuqp cr0, v1, v2",
            0xfc01_1508,
            "vsx300_35",
            [Reg("vs33"), Reg("vs34")],
        ),
        (
            "xststdcdp cr0, vs1, 127",
            0xf07f_0da8,
            "vsx300_74",
            [Reg("vs1"), Imm(127)],
        ),
        (
            "xststdcqp cr0, v1, 127",
            0xfc7f_0d88,
            "vsx300_75",
            [Reg("vs33"), Imm(127)],
        ),
        (
            "xststdcsp cr0, vs1, 127",
            0xf07f_0ca8,
            "vsx300_76",
            [Reg("vs1"), Imm(127)],
        ),
    ]
};

/// Whether `op`'s operands are `expected`.
fn operands_are(arch: Arch, f: &Function, op: NodeId, expected: &[Operand]) -> bool {
    let got = operands(f, op);
    got.len() == expected.len()
        && got.iter().zip(expected).all(|(&v, e)| match *e {
            Operand::Reg(name) => Some(v) == initial(arch, f, name),
            Operand::Imm(imm) => f.int_const_u128(v) == Some(imm),
        })
}

/// `<prelude>; <body>; b<cond> 1f; li r3, 1; blr; 1: li r3, 2; blr`
fn branch_after(arch: Arch, prelude: &[u32], body: u32, branch: u32) -> Function {
    let mut words = prelude.to_vec();
    words.extend([body, branch, LI_R3_1, BLR, LI_R3_2, BLR]);
    analyze(arch, &words)
}

/// `li r4, 0; cmpwi r4, 0; cmpwi cr6, r4, 0`: EQ set in cr0 and cr6.
const BOTH_EQ: [u32; 3] = [0x3880_0000, 0x2c04_0000, 0x2f04_0000];
/// `beq crN, +12`
const fn beq(field: u32) -> u32 {
    0x4182_000c | (field << 18)
}
const BOTH: [Option<u128>; 2] = [Some(1), Some(2)];
const TAKEN: [Option<u128>; 1] = [Some(2)];
const NOT_TAKEN: [Option<u128>; 1] = [Some(1)];

/// Each compare, as `cr0` and as `cr6` (`BF << 23`), sets the field it names
/// from its operands and leaves the other field. Both fields start with EQ
/// set, so a `beq` on the field it does not write always branches.
#[test]
fn a_vsx_compare_writes_its_condition_register_field() {
    let mut wrong = Vec::new();
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        for (name, cr0_form, op, args) in COMPARES {
            let cr6_form = cr0_form | (6 << 23);
            for (bf, word, beq_field, expected) in [
                (0, cr0_form, 0, &BOTH[..]),
                (6, cr6_form, 6, &BOTH[..]),
                (6, cr6_form, 0, &TAKEN[..]),
                (0, cr0_form, 6, &TAKEN[..]),
            ] {
                let f = branch_after(arch, &BOTH_EQ, word, beq(beq_field));
                let got = returned_r3s(&f);
                if got != expected {
                    wrong.push(format!(
                        "{arch:?} {name} as cr{bf}, beq cr{beq_field}: {got:?}"
                    ));
                }
                if bf == beq_field && !operands_are(arch, &f, the_call_other(&f, op), &args) {
                    wrong.push(format!("{arch:?} {name} as cr{bf}: operands of {op}"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each compare sets FPSCR.FPCC (field 4) as it sets CR field BF, and the
/// quad-precision compares raise VXSNAN (field 1, bit 3), `xscmpoqp` VXVC too
/// (field 3, bit 0): `mtfsfi F, 0; <compare>; mcrfs cr7, F; b<bit> cr7, 1f;
/// li r3, 1; blr; 1: li r3, 2; blr` keeps both paths for a bit the compare
/// may set, where the cleared field would never branch.
#[test]
fn a_vsx_compare_sets_the_fpscr_bits_it_writes() {
    // (mtfsfi F,0, mcrfs cr7,F, the branch on the bit)
    let fpcc = (0xfe00_010c, 0xff90_0080, 0x419e_000c, "FPCC.FE (beq)");
    let vxsnan = (0xfc80_010c, 0xff84_0080, 0x419f_000c, "VXSNAN (bso)");
    let vxvc = (0xfd80_010c, 0xff8c_0080, 0x419c_000c, "VXVC (blt)");
    let mut wrong = Vec::new();
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        for (name, word, _, _) in COMPARES {
            let mut bits = vec![fpcc];
            if name.starts_with("xscmpoqp") || name.starts_with("xscmpuqp") {
                bits.push(vxsnan);
            }
            if name.starts_with("xscmpoqp") {
                bits.push(vxvc);
            }
            for (clear, read, branch, bit) in bits {
                let got = returned_r3s(&fpscr_branch(arch, clear, word, read, branch));
                if got != BOTH {
                    wrong.push(format!("{arch:?} {name}, {bit}: {got:?}"));
                }
            }
        }
        // The cleared field alone never branches.
        let got = returned_r3s(&fpscr_branch(arch, fpcc.0, 0x6000_0000, fpcc.1, fpcc.2));
        if got != NOT_TAKEN {
            wrong.push(format!("{arch:?} nop, FPCC.FE: {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An FPSCR bit as `(mtfsfi F,0, mcrfs cr7,F, the branch on the bit, name)`.
type FpscrBit = (u32, u32, u32, &'static str);

const FPCC_FE: FpscrBit = (0xfe00_010c, 0xff90_0080, 0x419e_000c, "FPCC.FE");
const XX: FpscrBit = (0xfc80_010c, 0xff84_0080, 0x419e_000c, "XX");
const ZX: FpscrBit = (0xfc80_010c, 0xff84_0080, 0x419d_000c, "ZX");
const VXSNAN: FpscrBit = (0xfc80_010c, 0xff84_0080, 0x419f_000c, "VXSNAN");
const VXISI: FpscrBit = (0xfd00_010c, 0xff88_0080, 0x419c_000c, "VXISI");
const VXVC: FpscrBit = (0xfd80_010c, 0xff8c_0080, 0x419c_000c, "VXVC");
const FR: FpscrBit = (0xfd80_010c, 0xff8c_0080, 0x419d_000c, "FR");
const VXSQRT: FpscrBit = (0xfe80_010c, 0xff94_0080, 0x419e_000c, "VXSQRT");
const VXCVI: FpscrBit = (0xfe80_010c, 0xff94_0080, 0x419f_000c, "VXCVI");

/// The VSX arithmetic, rounding and conversion forms set the FPSCR status
/// bits the ISA lists for them (FPRF, FR, FI, FX, OX, UX, ZX, XX, VX*), the
/// vector conversions leave FPRF, and the XT-form compares and max/min set
/// only the invalid-operation bits. `mtfsfi F, 0; <op>; mcrfs cr7, F;
/// b<bit> cr7` keeps both paths for a bit the op may set, and never branches
/// for one it leaves, as for the permutes, moves and exponent and sign ops,
/// which set none. A left bit is probed only in a field the op sets nothing
/// in: `mcrfs` packs the field from byte registers, so an unknown bit beside
/// it makes the whole field unknown.
#[test]
fn a_vsx_op_sets_the_fpscr_status_bits_it_raises() {
    let sets: &[(&str, u32, &[FpscrBit], &[FpscrBit])] = &[
        (
            "xsaddqp",
            0xfc43_2008,
            &[FPCC_FE, XX, VXSNAN, VXISI, FR],
            &[],
        ),
        ("xsaddsp", 0xf022_1800, &[FPCC_FE, XX, VXSNAN, VXISI], &[]),
        // Inline p-code, flagged as `fadd` is.
        ("xsadddp", 0xf022_1900, &[FPCC_FE, VXSNAN], &[]),
        ("xsdivqp", 0xfc43_2448, &[FPCC_FE, ZX], &[]),
        ("xssqrtqp", 0xfc5b_1e48, &[FPCC_FE, VXSQRT], &[]),
        ("xscvqpswz", 0xfc49_1e88, &[XX, VXCVI], &[]),
        ("xsrqpi", 0xfc40_180a, &[FPCC_FE, VXSNAN], &[]),
        ("xscvdpqp", 0xfc56_1e88, &[FPCC_FE, VXSNAN], &[]),
        ("xscvudqp", 0xfc42_1e88, &[FPCC_FE], &[]),
        ("xscvhpdp", 0xf030_156c, &[FPCC_FE, VXSNAN], &[]),
        ("xscvsxdsp", 0xf020_14e0, &[FPCC_FE, XX], &[]),
        ("xsrsqrtesp", 0xf020_1028, &[FPCC_FE, ZX], &[]),
        ("xvcvsphp", 0xf039_176c, &[XX, VXSNAN], &[FPCC_FE]),
        ("xvcvhpsp", 0xf038_176c, &[VXSNAN], &[FPCC_FE]),
        ("xscmpgedp", 0xf022_1898, &[VXSNAN, VXVC], &[FPCC_FE]),
        ("xscmpeqdp", 0xf022_1818, &[VXSNAN], &[FPCC_FE]),
        // A draft encoding only the sla decodes.
        ("xvcmpnedp", 0xf022_1bd8, &[VXSNAN], &[FPCC_FE]),
        ("xsmaxcdp", 0xf022_1c00, &[VXSNAN], &[FPCC_FE]),
        ("xsminjdp", 0xf022_1cc0, &[VXSNAN], &[FPCC_FE]),
        ("xxperm", 0xf022_18d0, &[], &[FPCC_FE, VXSNAN]),
        ("xxbrd", 0xf037_176c, &[], &[FPCC_FE, VXSNAN]),
        ("xsabsqp", 0xfc40_1e48, &[], &[FPCC_FE, VXSNAN]),
        ("xsnegqp", 0xfc50_1e48, &[], &[FPCC_FE, VXSNAN]),
        ("xsiexpdp", 0xf023_272c, &[], &[FPCC_FE, VXSNAN]),
        ("xsxexpqp", 0xfc42_1e48, &[], &[FPCC_FE, VXSNAN]),
        ("xvtstdcdp", 0xf020_17a8, &[], &[FPCC_FE, VXSNAN]),
        ("xscvspdpn", 0xf020_152c, &[], &[FPCC_FE, VXSNAN]),
        ("mtvsrdd", 0x7c23_2366, &[], &[FPCC_FE, VXSNAN]),
    ];
    let mut wrong = Vec::new();
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        for &(name, word, raised, left) in sets {
            for (bits, expected) in [(raised, &BOTH[..]), (left, &NOT_TAKEN[..])] {
                for &(clear, read, branch, bit) in bits {
                    let got = returned_r3s(&fpscr_branch(arch, clear, word, read, branch));
                    if got != expected {
                        wrong.push(format!("{arch:?} {name}, {bit}: {got:?}"));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `xvcmpnedp.`, a draft encoding only the sla decodes, sets CR6 as well, as
/// `xvcmpeqdp.` does: with EQ set in cr6 beforehand, a `beq cr6` after it
/// keeps both paths.
#[test]
fn xvcmpnedp_dot_writes_cr6() {
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = branch_after(arch, &BOTH_EQ, 0xf022_1fd8, beq(6));
        assert_eq!(returned_r3s(&f), BOTH, "{arch:?}");
    }
}

/// `mtfsfi F, 0; <body>; mcrfs cr7, F; b<bit> cr7, 1f; li r3, 1; blr; 1: li
/// r3, 2; blr`
fn fpscr_branch(arch: Arch, clear: u32, body: u32, read: u32, branch: u32) -> Function {
    branch_after(arch, &[clear, body], read, branch)
}

/// `li r5, 0; cmpwi r5, 0; paste. r3, r4; beq 1f; li r3, 1; blr; 1: li r3,
/// 2; blr`: CR0 reports whether the paste succeeded, whatever the compare
/// before it left there.
#[test]
fn paste_dot_sets_cr0_from_the_paste() {
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = branch_after(arch, &[0x38a0_0000, 0x2c05_0000], 0x7c23_270d, beq(0));
        assert_eq!(returned_r3s(&f), BOTH, "{arch:?}");
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
        let op = sliced_from_call_other(&f, stored, "altv207_24")
            .unwrap_or_else(|| panic!("{arch:?}: v2 is {:?}", f.kind_of_value(stored)));
        assert_eq!(
            operands(&f, op).into_iter().map(Some).collect::<Vec<_>>(),
            [initial(arch, &f, "vs35"), initial(arch, &f, "vs36")],
            "{arch:?}: vminud reads v3 and v4"
        );
    }
}

/// `xscvudqp v2, v4; xsdivqp v2, v2, v3; blr`: the conversion and the divide
/// are two user ops, each with its own operands.
#[test]
fn xscvudqp_and_xsdivqp_are_distinct_user_ops() {
    for arch in [Arch::Ppc64be, Arch::Ppc64le] {
        let f = analyze(arch, &[0xfc42_2688, 0xfc42_1c48, BLR]);
        let convert = the_call_other(&f, "vsx300_47");
        let divide = the_call_other(&f, "vsx300_xsdivqp");
        assert_eq!(operands(&f, convert).len(), 1, "{arch:?} xscvudqp");
        assert_eq!(operands(&f, divide).len(), 2, "{arch:?} xsdivqp");
        // The new op is the last of the 877, so no other id moved.
        let (strider, _) = common::strider_over_bytes(arch, vec![0; 4], BASE, None);
        let names = strider.user_op_names();
        assert_eq!(names.len(), 877, "{arch:?}");
        assert_eq!(names[876], "vsx300_xsdivqp", "{arch:?}");
        assert_eq!(names[875], "altv300_71", "{arch:?}");
        assert_eq!(names[373], "vsx300_47", "{arch:?}");
    }
}
