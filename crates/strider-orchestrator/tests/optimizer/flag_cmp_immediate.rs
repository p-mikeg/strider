//! A signed compare against an immediate reaches the same single `Sless` a
//! register compare does, on real instruction bytes, and the branch it feeds
//! keeps its meaning.
//!
//! Each image is `cmp <reg>, #N; b<cond> taken; <ret 0>; taken: <ret 1>`.
//! The raw and the optimised lift both run on the reference interpreter, and
//! the returned 0/1 has to be the signed comparison of the register's low
//! `width` bits against `N`. x86-64 `cmp dil, imm8` is checked over every
//! `(x, N)` pair at 8 bits; the wider forms over a boundary grid.

use crate::common::{Arch, driver_for_reader};
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, FunctionBound, MachineInsnAddr};
use strider_ir::node::NodeKind;
use strider_ir::{Function, IRViewer, IRWalker, IntCmpOp};
use strider_ir_test_utils::interp::{self, EntryVar, Env, Outcome, Slot};

const BASE: u64 = 0x1000;
const STACK: u128 = 0x7fff_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cond {
    Lt,
    Ge,
    Gt,
    Le,
}

const CONDS: [Cond; 4] = [Cond::Lt, Cond::Ge, Cond::Gt, Cond::Le];

impl Cond {
    fn holds(self, x: i128, n: i128) -> bool {
        match self {
            Cond::Lt => x < n,
            Cond::Ge => x >= n,
            Cond::Gt => x > n,
            Cond::Le => x <= n,
        }
    }

    /// The x86 `jcc rel8` opcode.
    fn x86(self) -> u8 {
        match self {
            Cond::Lt => 0x7c,
            Cond::Ge => 0x7d,
            Cond::Le => 0x7e,
            Cond::Gt => 0x7f,
        }
    }

    /// The ARM / AArch64 condition field.
    fn arm(self) -> u32 {
        match self {
            Cond::Ge => 0b1010,
            Cond::Lt => 0b1011,
            Cond::Gt => 0b1100,
            Cond::Le => 0b1101,
        }
    }
}

/// One image to lift: `reg`'s low `width` bits, plus `pre_add` when the
/// image adds it first, compared against `n`.
struct Case {
    arch: Arch,
    bytes: Vec<u8>,
    reg: &'static str,
    width: u32,
    n: i128,
    cond: Cond,
    pre_add: u128,
}

fn le32(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn le16(halves: &[u16]) -> Vec<u8> {
    halves.iter().flat_map(|h| h.to_le_bytes()).collect()
}

/// `jcc +3; xor eax, eax; ret; mov eax, 1; ret` after `cmp`.
fn x86_tail(cmp: &[u8], cond: Cond) -> Vec<u8> {
    let mut v = cmp.to_vec();
    v.extend_from_slice(&[cond.x86(), 0x03, 0x31, 0xc0, 0xc3]);
    v.extend_from_slice(&[0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3]);
    v
}

/// x86-64 `cmp dil, imm8`.
fn x64_dil(n: u8, cond: Cond) -> Case {
    Case {
        arch: Arch::X64,
        bytes: x86_tail(&[0x40, 0x80, 0xff, n], cond),
        reg: "RDI",
        width: 8,
        n: i128::from(n as i8),
        cond,
        pre_add: 0,
    }
}

/// x86-64 `cmp edi, imm32`.
fn x64_edi(n: i32, cond: Cond) -> Case {
    let mut cmp = vec![0x81, 0xff];
    cmp.extend_from_slice(&n.to_le_bytes());
    Case {
        arch: Arch::X64,
        bytes: x86_tail(&cmp, cond),
        reg: "RDI",
        width: 32,
        n: i128::from(n),
        cond,
        pre_add: 0,
    }
}

/// x86-64 `cmp rdi, imm32`, sign-extended.
fn x64_rdi(n: i32, cond: Cond) -> Case {
    let mut cmp = vec![0x48, 0x81, 0xff];
    cmp.extend_from_slice(&n.to_le_bytes());
    Case {
        arch: Arch::X64,
        bytes: x86_tail(&cmp, cond),
        reg: "RDI",
        width: 64,
        n: i128::from(n),
        cond,
        pre_add: 0,
    }
}

/// x86-64 `cmp di, imm16`.
fn x64_di(n: i16, cond: Cond) -> Case {
    let mut cmp = vec![0x66, 0x81, 0xff];
    cmp.extend_from_slice(&n.to_le_bytes());
    Case {
        arch: Arch::X64,
        bytes: x86_tail(&cmp, cond),
        reg: "RDI",
        width: 16,
        n: i128::from(n),
        cond,
        pre_add: 0,
    }
}

/// x86-64 `cmp edi, imm8`, sign-extended.
fn x64_edi_imm8(n: i8, cond: Cond) -> Case {
    Case {
        arch: Arch::X64,
        bytes: x86_tail(&[0x83, 0xff, n as u8], cond),
        reg: "RDI",
        width: 32,
        n: i128::from(n),
        cond,
        pre_add: 0,
    }
}

/// i386 `cmp ecx, imm32`.
fn x86_ecx(n: i32, cond: Cond) -> Case {
    let mut cmp = vec![0x81, 0xf9];
    cmp.extend_from_slice(&n.to_le_bytes());
    Case {
        arch: Arch::X86,
        bytes: x86_tail(&cmp, cond),
        reg: "ECX",
        width: 32,
        n: i128::from(n),
        cond,
        pre_add: 0,
    }
}

/// An ARM modified immediate: `imm8` rotated right by `2 * rot`.
fn arm_imm12(v: u32) -> u32 {
    (0..16)
        .find_map(|rot| {
            let imm8 = v.rotate_left(2 * rot);
            (imm8 <= 0xff).then_some((rot << 8) | imm8)
        })
        .expect("ARM-encodable immediate")
}

/// ARM `cmp r0, #imm` (or `cmn r0, #imm`, which compares against `-imm`).
fn arm(imm: u32, cmn: bool, cond: Cond) -> Case {
    let op = if cmn { 0xe370_0000 } else { 0xe350_0000 };
    let words = [
        op | arm_imm12(imm),
        (cond.arm() << 28) | 0x0a00_0001,
        0xe3a0_0000, // mov r0, #0
        0xe12f_ff1e, // bx lr
        0xe3a0_0001, // mov r0, #1
        0xe12f_ff1e,
    ];
    let n = i128::from(imm as i32);
    Case {
        arch: Arch::Arm,
        bytes: le32(&words),
        reg: "r0",
        width: 32,
        n: if cmn { -n } else { n },
        cond,
        pre_add: 0,
    }
}

/// Thumb `b<cond> +2; movs r0, #0; bx lr; movs r0, #1; bx lr` after `cmp`.
fn thumb_tail(cmp: &[u16], cond: Cond) -> Vec<u8> {
    let mut h = cmp.to_vec();
    h.extend_from_slice(&[
        0xd001 | ((cond.arm() as u16) << 8),
        0x2000,
        0x4770,
        0x2001,
        0x4770,
    ]);
    le16(&h)
}

/// Thumb `cmp r0, #imm8`.
fn thumb(imm: u8, cond: Cond) -> Case {
    Case {
        arch: Arch::ArmThumb,
        bytes: thumb_tail(&[0x2800 | u16::from(imm)], cond),
        reg: "r0",
        width: 32,
        n: i128::from(imm),
        cond,
        pre_add: 0,
    }
}

/// Thumb-2 `cmp.w r0, #0x80000000`.
fn thumb_int_min(cond: Cond) -> Case {
    Case {
        arch: Arch::ArmThumb,
        bytes: thumb_tail(&[0xf1b0, 0x4f00], cond),
        reg: "r0",
        width: 32,
        n: i128::from(i32::MIN),
        cond,
        pre_add: 0,
    }
}

/// Thumb-2 `cmn.w r0, #imm8`, a compare against `-imm8`.
fn thumb_cmn(imm: u8, cond: Cond) -> Case {
    Case {
        arch: Arch::ArmThumb,
        bytes: thumb_tail(&[0xf110, 0x0f00 | u16::from(imm)], cond),
        reg: "r0",
        width: 32,
        n: -i128::from(imm),
        cond,
        pre_add: 0,
    }
}

/// AArch64 `cmp w0|x0, #imm12` (or `cmn`, a compare against `-imm12`).
fn a64(imm: u16, wide: bool, cmn: bool, cond: Cond) -> Case {
    let op = match (wide, cmn) {
        (false, false) => 0x7100_001f,
        (true, false) => 0xf100_001f,
        (false, true) => 0x3100_001f,
        (true, true) => 0xb100_001f,
    };
    let words = [
        op | (u32::from(imm) << 10),
        0x5400_0000 | (3 << 5) | cond.arm(),
        0x5280_0000, // mov w0, #0
        0xd65f_03c0, // ret
        0x5280_0020, // mov w0, #1
        0xd65f_03c0,
    ];
    Case {
        arch: Arch::Aarch64,
        bytes: le32(&words),
        reg: "x0",
        width: if wide { 64 } else { 32 },
        n: if cmn {
            -i128::from(imm)
        } else {
            i128::from(imm)
        },
        cond,
        pre_add: 0,
    }
}

/// `case` with `add <reg>, #c` before its compare.
fn after_add(mut case: Case, c: u8) -> Case {
    let add = match case.arch {
        Arch::X64 => vec![0x83, 0xc7, c],
        Arch::X86 => vec![0x83, 0xc1, c],
        Arch::Arm => (0xe280_0000u32 | u32::from(c)).to_le_bytes().to_vec(),
        Arch::ArmThumb => (0x3000u16 | u16::from(c)).to_le_bytes().to_vec(),
        Arch::Aarch64 => (0x1100_0000u32 | (u32::from(c) << 10))
            .to_le_bytes()
            .to_vec(),
        other => panic!("no add for {}", other.name()),
    };
    case.bytes.splice(0..0, add);
    case.pre_add = u128::from(c);
    case
}

/// The raw and the optimised lift of each of `cases`, all of one arch, laid
/// out back to back under one lifter, and the compared register.
fn lift(cases: &[Case]) -> (Vec<[Function; 2]>, rsleigh::Vn) {
    let arch = cases[0].arch;
    let image: Vec<u8> = cases.iter().flat_map(|c| c.bytes.iter().copied()).collect();
    let (mut lifter, cc) = driver_for_reader(arch, BufMemReader::new(image, BASE));
    let reg = lifter
        .sleigh_regs()
        .name_to_vn(cases[0].reg)
        .unwrap_or_else(|| panic!("{} names no register", cases[0].reg));
    let mut at = BASE;
    let mut lifts = Vec::new();
    for case in cases {
        assert_eq!((case.arch, case.reg), (arch, cases[0].reg));
        let size = case.bytes.len() as u64;
        let opts = CfgOptions {
            bound: FunctionBound::size(size),
            ..Default::default()
        };
        // Bit 0 of a Thumb entry selects the instruction set.
        let entry = if arch == Arch::ArmThumb { at | 1 } else { at };
        let cfg = lifter
            .build_cfg(MachineInsnAddr::from(entry), &opts)
            .expect("cfg");
        let raw = lifter
            .build_ir(&cfg, cc.clone())
            .expect("build_ir")
            .function;
        let mut opt = raw.clone();
        strider_orchestrator::opt::default_pipeline()
            .run(&mut opt, &strider_orchestrator::opt::OptCtx::new(None))
            .expect("pipeline");
        lifts.push([raw, opt]);
        at += size;
    }
    (lifts, reg)
}

fn mask(width: u32) -> u128 {
    if width == 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    }
}

fn sext(v: u128, width: u32) -> i128 {
    let shift = 128 - width;
    ((v << shift) as i128) >> shift
}

/// The 0/1 `f` returns with `x` in the compared register, whose bits above
/// `width` hold noise.
fn returned(f: &Function, reg: rsleigh::Vn, stack: rsleigh::Vn, x: u128, width: u32) -> u128 {
    let noise = 0x5a5a_a5a5_c3c3_3c3c_0f0f_f0f0_9696_6969u128 & !mask(width);
    let entry = |e: EntryVar| {
        if e.vn.addr_space == reg.addr_space && e.vn.addr_off == reg.addr_off {
            noise | (x & mask(width))
        } else if e.vn == stack {
            STACK
        } else {
            0
        }
    };
    let env = Env {
        entry: &entry,
        rom: None,
        seed: 0,
        step_budget: 64,
        private: None,
    };
    match interp::run(f, &env) {
        Ok(Outcome::Return { values, .. }) => match values.first() {
            Some(Slot::Known(v)) => *v & 0xffff_ffff,
            other => panic!("return slot {other:?}"),
        },
        other => panic!("run: {other:?}"),
    }
}

/// The single `If` tests `Sless` of the register against `N`, in the order
/// `cond` names.
fn assert_canonical(case: &Case, opt: &Function) {
    let ifs: Vec<_> = opt
        .walk()
        .filter(|&n| matches!(opt.node_kind(n), NodeKind::If))
        .collect();
    assert_eq!(ifs.len(), 1, "{case:?}: one If\n{}", opt.to_text(false));
    let cond = opt.producer(opt.if_cond(ifs[0]));
    assert_eq!(
        *opt.node_kind(cond),
        NodeKind::IntCmpOp(IntCmpOp::Sless),
        "{case:?}: the branch tests\n{}",
        opt.to_text(false)
    );
    let [l, r]: [_; 2] = opt
        .node_inputs(cond)
        .into_iter()
        .collect::<Vec<_>>()
        .try_into()
        .expect("two operands");
    let n = (case.n as u128) & mask(case.width);
    // Lt / Ge test `x < N`; Gt / Le test `N < x`.
    let (c, other) = match case.cond {
        Cond::Lt | Cond::Ge => (r, l),
        Cond::Gt | Cond::Le => (l, r),
    };
    assert_eq!(
        opt.int_const_u128(c),
        Some(n),
        "{case:?}: the constant operand\n{}",
        opt.to_text(false)
    );
    assert_eq!(
        opt.int_const_u128(other),
        None,
        "{case:?}: the register operand"
    );
}

impl std::fmt::Debug for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:?} {}+{} N={} (i{})",
            self.arch.name(),
            self.cond,
            self.reg,
            self.pre_add,
            self.n,
            self.width
        )
    }
}

/// `x` values around the interesting points of a `width`-bit compare
/// against `n`.
fn grid(n: i128, width: u32) -> Vec<u128> {
    let min = -(1i128 << (width - 1));
    let max = (1i128 << (width - 1)) - 1;
    let mut xs = vec![0, 1, -1, 2, -2, min, min + 1, max, max - 1];
    for d in -2..=2 {
        xs.push(n + d);
        xs.push(-n + d);
    }
    xs.extend([0x1234_5678, -0x1234_5678, 0x7ffe, -0x8001]);
    xs.into_iter().map(|x| (x as u128) & mask(width)).collect()
}

/// Per case: the canonical form where `N` fits the width (`cmn
/// #0x80000000` compares against 2^31, which no `Sless` names), and both lifts
/// agree with the reference on `xs(case)`.
fn check(cases: &[Case], xs: impl Fn(&Case) -> Vec<u128>) {
    let (lifts, reg) = lift(cases);
    for (case, [raw, opt]) in cases.iter().zip(&lifts) {
        if sext((case.n as u128) & mask(case.width), case.width) == case.n {
            assert_canonical(case, opt);
        }
        let stack = opt.default_cc().stack_vn;
        for x in xs(case) {
            let compared = x.wrapping_add(case.pre_add) & mask(case.width);
            let want = u128::from(case.cond.holds(sext(compared, case.width), case.n));
            for (which, f) in [("raw", raw), ("optimised", opt)] {
                assert_eq!(
                    returned(f, reg, stack, x, case.width),
                    want,
                    "{case:?}: {which} at x={x:#x}"
                );
            }
        }
    }
}

/// [`grid`] about `N`, and about where `x + pre_add` reaches `N`.
fn check_grid(cases: &[Case]) {
    check(cases, |case| {
        let mut xs = grid(case.n, case.width);
        xs.extend(grid(case.n - case.pre_add as i128, case.width));
        xs
    });
}

#[test]
fn x64_cmp_dil_imm8_every_pair() {
    let cases: Vec<Case> = CONDS
        .into_iter()
        .flat_map(|cond| (0..=255u8).map(move |n| x64_dil(n, cond)))
        .collect();
    check(&cases, |_| (0..=255).collect());
}

#[test]
fn x64_cmp_edi_imm() {
    let mut cases = Vec::new();
    for cond in CONDS {
        for n in [0, 5, -7, 0x7fff_ffff, i32::MIN, 1000] {
            cases.push(x64_edi(n, cond));
        }
        for n in [0, 5, -7, -128, 127] {
            cases.push(x64_edi_imm8(n, cond));
        }
        for n in [0, -7, i32::MIN] {
            cases.push(x64_rdi(n, cond));
        }
        for n in [0, 5, -7, 0x7fff, -0x8000] {
            cases.push(x64_di(n, cond));
        }
    }
    check_grid(&cases);
}

#[test]
fn x86_cmp_ecx_imm() {
    let mut cases = Vec::new();
    for cond in CONDS {
        for n in [0, 5, -7, 0x7fff_ffff, i32::MIN] {
            cases.push(x86_ecx(n, cond));
        }
    }
    check_grid(&cases);
}

#[test]
fn arm_cmp_r0_imm() {
    let mut cases = Vec::new();
    for cond in CONDS {
        for imm in [0, 5, 0xff, 0x8000_0000] {
            cases.push(arm(imm, false, cond));
        }
        for imm in [0, 5, 0xff, 0x8000_0000] {
            cases.push(arm(imm, true, cond));
        }
    }
    check_grid(&cases);
}

#[test]
fn thumb_cmp_r0_imm() {
    let mut cases = Vec::new();
    for cond in CONDS {
        for imm in [0, 5, 0xff] {
            cases.push(thumb(imm, cond));
        }
        cases.push(thumb_int_min(cond));
        cases.push(thumb_cmn(5, cond));
    }
    check_grid(&cases);
}

#[test]
fn aarch64_cmp_imm() {
    let mut cases = Vec::new();
    for cond in CONDS {
        for wide in [false, true] {
            for imm in [0, 5, 4095] {
                cases.push(a64(imm, wide, false, cond));
            }
            for imm in [0, 5, 4095] {
                cases.push(a64(imm, wide, true, cond));
            }
        }
    }
    check_grid(&cases);
}

/// `i + 1` compared against an immediate: ConstantFold reassociates the
/// flags' `Add(Add(i, 1), -N)` into `Add(i, 1 - N)`.
#[test]
fn cmp_after_an_add_of_an_immediate() {
    let mut cases: [Vec<Case>; 5] = Default::default();
    for cond in CONDS {
        for (c, n) in [(1u8, 15u8), (1, 0), (3, 0x7f)] {
            cases[0].push(after_add(x64_edi_imm8(n as i8, cond), c));
            cases[1].push(after_add(x86_ecx(i32::from(n), cond), c));
            cases[2].push(after_add(arm(u32::from(n), false, cond), c));
            cases[3].push(after_add(thumb(n, cond), c));
            cases[4].push(after_add(a64(u16::from(n), false, false, cond), c));
        }
    }
    for arch_cases in &cases {
        check_grid(arch_cases);
    }
}
