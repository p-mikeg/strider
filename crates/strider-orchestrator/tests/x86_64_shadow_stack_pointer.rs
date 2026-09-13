//! `RDSSPD r32` reads the low half of SSP into a zero-extended 32-bit
//! destination, and `INCSSPD`/`INCSSPQ` advance SSP by the low byte of their
//! operand times the pointer size, without wrapping in a byte.

mod common;

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{ExtendOp, Function, IRViewer, IRWalker, IntBinaryOp};

const BASE: u64 = 0x10000;

fn analyze(bytes: Vec<u8>) -> Function {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze")
        .function
}

fn returned_rax(f: &Function) -> ValueId {
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.node_inputs(ret)[2]
}

#[test]
fn rdsspd_zero_extends_the_shadow_stack_pointer_into_eax() {
    // rdsspd eax; ret
    let f = analyze(vec![0xf3, 0x0f, 0x1e, 0xc8, 0xc3]);
    let rax = returned_rax(&f);
    assert_eq!(
        *f.node_kind(f.producer(rax)),
        NodeKind::Extend(ExtendOp::ZeroExtend),
        "rax is {:?}",
        f.node_kind(f.producer(rax))
    );
}

#[test]
fn incssp_scales_the_whole_low_byte() {
    // mov eax, 0x80; incssp{d,q} {eax,rax}; rdsspq rax; ret
    for (name, incssp, step) in [
        ("incsspd", &[0xf3, 0x0f, 0xae, 0xe8][..], 0x200u128),
        ("incsspq", &[0xf3, 0x48, 0x0f, 0xae, 0xe8], 0x400),
    ] {
        let mut bytes = vec![0xb8, 0x80, 0, 0, 0];
        bytes.extend(incssp);
        bytes.extend([0xf3, 0x48, 0x0f, 0x1e, 0xc8, 0xc3]);
        let f = analyze(bytes);
        let rax = returned_rax(&f);
        assert_eq!(
            *f.node_kind(f.producer(rax)),
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            "{name}: rax is {:?}",
            f.node_kind(f.producer(rax))
        );
        let operands: Vec<ValueId> = f.node_inputs(f.producer(rax)).into_iter().collect();
        assert!(
            operands.iter().any(|&v| f.int_const_u128(v) == Some(step)),
            "{name}: SSP does not advance by {step:#x}"
        );
    }
}
