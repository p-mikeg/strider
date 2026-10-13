//! A `CallOther` whose pcode output is a sub-register writes that slice, not
//! the whole enclosing register: `in ax,dx` leaves RAX bits 16..63 alone,
//! where widening the output to its container makes them the intrinsic's
//! opaque result.

use strider_ir::{IRViewer, IRWalker};
use strider_lift::LiftOptions;

fn lift(
    arch: strider_target::SleighArch,
    cc: strider_target::CallingConvention,
    bytes: Vec<u8>,
) -> strider_ir::Function {
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let cc = cc.build(lifter.sleigh_regs()).expect("cc");
    let opts = LiftOptions::default();
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
        .expect("cfg");
    lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function
}

/// The result slot of the one `CallOther`, and whether the value the function
/// returns in its first return register still depends on that register's
/// incoming value.
fn result_type_and_preserved(f: &strider_ir::Function) -> (strider_ir::ValueType, bool) {
    let node = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::CallOther { .. }))
        .next()
        .expect("a CallOther node");
    // Outputs are `[Control, Memory]` then the result.
    let result = f.node_outputs(node)[2];
    let ty = match f.value_kind(result) {
        strider_ir::node::ValueKind::Typed(ty) => ty,
        other => panic!("the result slot is {other:?}"),
    };
    let ret = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Return))
        .next()
        .expect("a Return node");
    let ret_val = f.nth_input(ret, 2).expect("a first return value");
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![ret_val];
    let mut preserved = false;
    while let Some(v) = stack.pop() {
        if !seen.insert(v) {
            continue;
        }
        let producer = f.producer(v);
        if matches!(
            f.node_kind(producer),
            strider_ir::node::NodeKind::InitialVar(_)
        ) {
            preserved = true;
        }
        stack.extend(f.node_inputs(producer));
    }
    (ty, preserved)
}

/// x86-64 `in ax,dx` writes AX; the SDM leaves RAX bits 16..63 untouched.
#[test]
fn an_x86_sub_register_intrinsic_output_preserves_the_rest() {
    // 1000: 66 ed       in ax,dx
    // 1002: c3          ret
    let f = lift(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        vec![0x66, 0xed, 0xc3],
    );
    let (ty, preserved) = result_type_and_preserved(&f);
    assert_eq!(
        ty,
        strider_ir::ValueType::I16,
        "the result slot is the explicit output's width",
    );
    assert!(
        preserved,
        "RAX bits 16..63 must still come from the incoming RAX",
    );
}

/// AArch64 zeroes the rest of a vector register and the sla emits that zext
/// itself, so a slice write there would read the incoming register for bytes
/// the machine redefines: the result slot stays the whole container.
#[test]
fn an_aarch64_sub_register_intrinsic_output_stays_the_container() {
    // 1000: 0e 20 48 20    cls v0.8b,v1.8b
    // 1004: d6 5f 03 c0    ret
    let f = lift(
        strider_target::SleighArch::aarch64(),
        strider_target::CallingConvention::aarch64_aapcs64(),
        vec![0x20, 0x48, 0x20, 0x0e, 0xc0, 0x03, 0x5f, 0xd6],
    );
    let node = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::CallOther { .. }))
        .next()
        .expect("a CallOther node");
    assert_eq!(
        f.value_kind(f.node_outputs(node)[2]),
        strider_ir::node::ValueKind::Typed(strider_ir::ValueType::I128),
        "the result slot is q0's width, not d0's",
    );
}

/// x86 MMX registers are slices of the x87 `ST` registers, whose sign and
/// exponent an MMX write redefines, and `ST0` is a float return register: a
/// slice write there would assert those bits unchanged and return them.
#[test]
fn an_x86_mmx_intrinsic_output_stays_the_x87_container() {
    // 1000: 0f 38 00 c1    pshufb mm0,mm1
    // 1004: c3             ret
    let f = lift(
        strider_target::SleighArch::x86_64(),
        strider_target::CallingConvention::x86_64_systemv(),
        vec![0x0f, 0x38, 0x00, 0xc1, 0xc3],
    );
    let node = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::CallOther { .. }))
        .next()
        .expect("a CallOther node");
    let result = f.node_outputs(node)[2];
    assert_eq!(
        f.value_kind(result),
        strider_ir::node::ValueKind::Typed(strider_ir::ValueType::I80),
        "the result slot is ST0's width, not MM0's",
    );
    let ret = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::Return))
        .next()
        .expect("a Return node");
    assert!(
        f.node_inputs(ret).iter().any(|v| v == result),
        "the x87 return slot is the intrinsic's result, not a masked insert \
         preserving the caller's sign and exponent",
    );
}
