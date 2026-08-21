//! Incoming float arguments get carriers, indexed in their own class.
//!
//! `double f64_arith(double, double)` receives its parameters in XMM0/XMM1
//! (x86-64), d0/d1 (ARM), q0/q1 (AArch64), f1/f2 (PowerPC), f12/f14 (MIPS).
//! It never touches an integer argument register, so all of its carriers come
//! from the float class.
//!
//! Indices are per class: the j-th float parameter is float carrier `j`, and
//! integer carriers keep their own numbering.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;
use strider_pattern::{Capture, Matcher, any_function_arg, function_arg, function_arg_float};

/// Every arch whose ABI passes `double` in a float register.
const FLOAT_ARG_ARCHES: &[Arch] = &[
    Arch::X64,
    Arch::Aarch64,
    Arch::Aarch64Be,
    Arch::Arm,
    Arch::ArmBe,
    Arch::ArmThumb,
    Arch::Mips32le,
    Arch::Mips32be,
    Arch::Mips64le,
    Arch::Mips64be,
    Arch::Ppc32be,
    Arch::Ppc32le,
    Arch::Ppc64be,
    Arch::Ppc64le,
];

/// Varnode containment, as `FunctionBuilder` canonicalisation applies it: a
/// carrier is the largest tracked container, so `q0` carries `d0`.
fn contains(container: &rsleigh::Vn, inner: &rsleigh::Vn) -> bool {
    container.addr_space == inner.addr_space
        && container.addr_off <= inner.addr_off
        && inner.addr_off + u64::from(inner.size) <= container.addr_off + u64::from(container.size)
}

/// The varnode an `InitialVar` carrier stands for, or `None` for a stack
/// carrier.
fn carrier_vn(f: &strider_ir::Function, value: strider_ir::node::ValueId) -> Option<rsleigh::Vn> {
    match f.node_kind(f.producer(value)) {
        NodeKind::InitialVar(id) => Some(f.initial_vn(*id)),
        _ => None,
    }
}

/// Register varnodes bound by every carrier `any_function_arg()` reaches.
fn any_carrier_vns(f: &strider_ir::Function) -> Vec<rsleigh::Vn> {
    let m = Matcher::new(f);
    let c = Capture::new();
    m.find_all(&any_function_arg().capture(c).build())
        .unwrap()
        .iter()
        .filter_map(|hit| carrier_vn(f, hit.value(c).expect("carrier capture is bound")))
        .collect()
}

/// The headline: the register holding the first `double` parameter must be a
/// registered carrier on every float-passing arch.
#[test]
fn first_float_parameter_has_a_carrier() {
    for &arch in FLOAT_ARG_ARCHES {
        let f = analyze(arch, "floats", "f64_arith");
        let want = f.default_cc().arg_passing_regs_float[0];
        let got = any_carrier_vns(&f);
        assert!(
            got.iter().any(|vn| contains(vn, &want)),
            "{}: no carrier holds the first float argument register \
             {want:?}; carriers bind {got:?}",
            arch.name(),
        );
    }
}

/// `function_arg_float(j)` binds the j-th float argument register's container,
/// and never an integer argument register.
#[test]
fn float_arg_index_binds_the_float_register() {
    for &arch in FLOAT_ARG_ARCHES {
        let f = analyze(arch, "floats", "f64_arith");
        let cc = f.default_cc();
        let m = Matcher::new(&f);
        // `double f64_arith(double, double)` fills float positions 0 and 1.
        for j in 0..2usize {
            let c = Capture::new();
            let hits = m
                .find_all(&function_arg_float(j as u32).capture(c).build())
                .unwrap();
            assert_eq!(
                hits.len(),
                1,
                "{}: function_arg_float({j}) must bind exactly one carrier",
                arch.name(),
            );
            let value = hits[0].value(c).expect("carrier capture is bound");
            let vn = carrier_vn(&f, value).expect("a float carrier is an InitialVar");
            assert!(
                contains(&vn, &cc.arg_passing_regs_float[j]),
                "{}: function_arg_float({j}) bound {vn:?}, which does not hold \
                 float argument register {:?}",
                arch.name(),
                cc.arg_passing_regs_float[j],
            );
            assert!(
                !cc.arg_passing_regs.iter().any(|r| contains(&vn, r)),
                "{}: function_arg_float({j}) bound integer argument register \
                 {vn:?}",
                arch.name(),
            );
        }
    }
}

/// Float carriers are numbered in their own space: `f64_arith`'s first
/// `double` is float argument 0, not argument 6 (x86-64's first free integer
/// position), and integer `function_arg(0)` never reaches it.
#[test]
fn float_and_integer_index_spaces_are_separate() {
    let f = analyze(Arch::X64, "floats", "f64_arith");
    let m = Matcher::new(&f);

    let float0 = Capture::new();
    let hits = m
        .find_all(&function_arg_float(0).capture(float0).build())
        .unwrap();
    let float_carrier = f.producer(hits[0].value(float0).expect("bound"));

    for i in 0..8u32 {
        for hit in m
            .find_all(&function_arg(i).capture(float0).build())
            .unwrap()
        {
            assert_ne!(
                f.producer(hit.value(float0).expect("bound")),
                float_carrier,
                "function_arg({i}) must not reach the float carrier",
            );
        }
    }
    // `any_function_arg` spans both classes.
    let any = Capture::new();
    let reached: Vec<_> = m
        .find_all(&any_function_arg().capture(any).build())
        .unwrap()
        .iter()
        .map(|hit| f.producer(hit.value(any).expect("bound")))
        .collect();
    assert!(
        reached.contains(&float_carrier),
        "any_function_arg() must reach the float carrier; reached {reached:?}",
    );
    assert!(
        reached.len() > 1,
        "any_function_arg() must still reach the integer carriers too",
    );
}

/// Integer carrier numbering is pinned: x86-64 SysV RDI, RSI, RDX, RCX, R8,
/// R9, then two stack slots.
#[test]
fn integer_arg_indices_are_unchanged() {
    let f = analyze(Arch::X64, "calling_convention", "forward_8");
    let expected: [&str; 8] = [
        "reg(0x38,8)",
        "reg(0x30,8)",
        "reg(0x10,8)",
        "reg(0x8,8)",
        "reg(0x80,8)",
        "reg(0x88,8)",
        "stack(8)",
        "stack(16)",
    ];
    for (i, want) in expected.iter().enumerate() {
        let mut got: Vec<String> = f
            .side_tables()
            .arg_index_to_values(i as u32)
            .iter()
            .map(|&v| {
                let n = f.producer(v);
                match f.node_kind(n) {
                    NodeKind::InitialVar(id) => {
                        let vn = f.initial_vn(*id);
                        format!("reg({:#x},{})", vn.addr_off, vn.size)
                    }
                    NodeKind::Load(_) => match f.stack_offset(n) {
                        Some((_, off)) => format!("stack({off})"),
                        None => "stack(?)".to_string(),
                    },
                    k => format!("{k:?}"),
                }
            })
            .collect();
        got.sort();
        assert_eq!(got, vec![want.to_string()], "integer arg {i} moved");
    }
    // The query surface agrees with the side table.
    let m = Matcher::new(&f);
    for i in 0..8u32 {
        assert_eq!(
            m.find_all(&function_arg(i).build()).unwrap().len(),
            1,
            "function_arg({i}) must bind exactly one carrier",
        );
    }
}
