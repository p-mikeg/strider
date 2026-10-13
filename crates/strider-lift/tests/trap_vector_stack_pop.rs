//! Only an `int`/`into` trap vector pushes no return address, so only a call
//! through the `swi` intrinsic's result pops nothing.  A call through a
//! register any other intrinsic wrote keeps the convention's pop.

use strider_ir::{IRViewer, IRWalker};
use strider_lift::LiftOptions;

/// The post-call stack adjust, read off `mov rax,rsp; ret`: `None` when the
/// returned RAX IS the `Call`'s own SP input, `Some(c)` when it is
/// `Add(sp, c)`.
fn call_stack_pop(bytes: Vec<u8>) -> Option<u128> {
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(lifter.sleigh_regs())
        .expect("systemv");
    let opts = LiftOptions::default();
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;

    let call = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Call { .. }))
        .next()
        .expect("a Call node");
    // `build_call`'s inputs are `[ctrl, mem, target, sp, args..]`.
    let sp = f.nth_input(call, 3).expect("the Call's SP input");
    let ret = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Return))
        .next()
        .expect("a Return node");
    // `build_return`'s inputs are `[ctrl, mem, retvals..]`, RAX first.
    let rax = f.nth_input(ret, 2).expect("the first return value");
    if rax == sp {
        return None;
    }
    let add = f.producer(rax);
    assert!(
        matches!(
            f.node_kind(add),
            strider_ir::node::NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add)
        ),
        "RAX must be the SP input or an Add over it, got {:?}",
        f.node_kind(add),
    );
    let [a, b] = f.node_inputs_exact(add).expect("two Add inputs");
    assert!(
        a == sp || b == sp,
        "the Add must be over the Call's SP input"
    );
    f.int_const_u128(if a == sp { b } else { a })
}

/// `int 0x80` lifts as `swi(); call <swi result>`: no return address was
/// pushed, so the callee pops none.
#[test]
fn swi_trap_vector_pops_nothing() {
    // 1000: cd 80          int 0x80
    // 1002: 48 89 e0       mov rax,rsp
    // 1005: c3             ret
    assert_eq!(
        call_stack_pop(vec![0xcd, 0x80, 0x48, 0x89, 0xe0, 0xc3]),
        None,
    );
}

/// `rdrand` is a `CallOther` too, but a `call rax` through its result is an
/// ordinary indirect call: the callee's `ret` pops the return address.
#[test]
fn call_through_another_intrinsics_result_keeps_the_pop() {
    // 1000: 48 0f c7 f0    rdrand rax
    // 1004: ff d0          call rax
    // 1006: 48 89 e0       mov rax,rsp
    // 1009: c3             ret
    assert_eq!(
        call_stack_pop(vec![
            0x48, 0x0f, 0xc7, 0xf0, 0xff, 0xd0, 0x48, 0x89, 0xe0, 0xc3,
        ]),
        Some(8),
        "SysV pops the pushed return address",
    );
}
