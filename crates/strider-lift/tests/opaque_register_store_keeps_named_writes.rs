//! An opaque REGISTER-space STORE re-reads every tracked register out of the
//! space, so each register's current value must be in its slot before the
//! store: a register written by name since the last opaque access would
//! otherwise read back whatever the slot held then.

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker};
use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

const BASE: u64 = 0x1000;

fn lift(
    arch: SleighArch,
    bytes: Vec<u8>,
    cc: CallingConvention,
    ret_reg: &str,
) -> (Function, ValueId, rsleigh::SleighRegs) {
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, BASE),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let regs = lifter.sleigh_regs().clone();
    let cc: BuiltCallingConvention = cc.build(&regs).expect("cc");
    let opts = strider_lift::LiftOptions::default();
    let cfg = lifter
        .build_cfg(BASE.into(), &opts.cfg, &opts.per_address_ccs)
        .expect("cfg");
    let f = lifter
        .build_ir_with(&cfg, cc.clone(), &opts)
        .expect("ir")
        .function;
    let ret = f
        .walk_kind(|k| matches!(k, NodeKind::Return))
        .next()
        .expect("a Return");
    let reg = regs.name_to_vn(ret_reg).expect("return register");
    let slot = 2 + cc
        .ret_val_regs
        .iter()
        .position(|v| *v == reg)
        .expect("a return register");
    let value = f.node_inputs(ret)[slot];
    (f, value, regs)
}

/// ppc32be. `mfsrin` mirrors the registers into the space while `r5` is still
/// its incoming value; `mtsrin` stores at `0x800 + (r4 >> 28)`, which folds to
/// no declared register.
#[test]
fn a_named_register_write_survives_an_opaque_register_store() {
    let bytes = vec![
        0x3c, 0x80, 0x30, 0x00, // lis    r4,0x3000
        0x7c, 0xe0, 0x25, 0x26, // mfsrin r7,r4
        0x38, 0xa0, 0x00, 0x07, // li     r5,7
        0x7c, 0xc0, 0x21, 0xe4, // mtsrin r6,r4
        0x7c, 0xa3, 0x2b, 0x78, // mr     r3,r5
        0x4e, 0x80, 0x00, 0x20, // blr
    ];
    let (f, r3, regs) = lift(
        SleighArch::ppc32be(),
        bytes,
        CallingConvention::powerpc_sysv32(),
        "r3",
    );
    let r5 = regs.name_to_vn("r5").expect("r5");

    // r3's whole cone, memory chain included.
    let mut seen = std::collections::HashSet::new();
    let mut work = vec![r3];
    let mut consts = Vec::new();
    let mut initial_r5_reaches = false;
    while let Some(v) = work.pop() {
        if !seen.insert(v) {
            continue;
        }
        let n = f.producer(v);
        consts.extend(f.int_const_u128(v));
        if let NodeKind::InitialVar(id) = f.node_kind(n)
            && f.initial_vn(*id) == r5
        {
            initial_r5_reaches = true;
        }
        if !matches!(f.node_kind(n), NodeKind::Entry) {
            work.extend(f.node_inputs(n));
        }
    }
    assert!(
        consts.contains(&7),
        "`li r5,7` never reaches r3 (initial r5 reaches: {initial_r5_reaches})\n{}",
        f.to_text(false),
    );
}

/// `mov r0,#7; vld2.32 {d4,d5},[r3]; bx lr`. The `vld2` stores through a
/// pointer its own p-code loop advances, so the address folds to no register.
/// Reading every opaque store as missing r0's slot, r0 at the return must be
/// exactly the 7: each re-read resolves through the chain to the nearest write
/// to that slot, never to the slot's initial contents.
fn r0_is_seven_unless_the_opaque_store_names_it(arch: SleighArch, big_endian_code: bool) {
    enum At {
        Value(ValueId),
        Memory(ValueId),
    }
    let insns: [u32; 3] = [0xe3a0_0007, 0xf423_488f, 0xe12f_ff1e];
    let bytes = insns
        .iter()
        .flat_map(|i| {
            if big_endian_code {
                i.to_be_bytes()
            } else {
                i.to_le_bytes()
            }
        })
        .collect();
    let (f, r0, regs) = lift(arch, bytes, CallingConvention::arm_aapcs_soft(), "r0");
    let slot = u128::from(regs.name_to_vn("r0").expect("r0").addr_off);
    let at_slot = |space: &rsleigh::VnSpace, addr| {
        *space == rsleigh::VnSpace::REGISTER && f.int_const_u128(addr) == Some(slot)
    };

    let mut values = Vec::new();
    let mut others = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut work = vec![At::Value(r0)];
    while let Some(at) = work.pop() {
        let (At::Value(v) | At::Memory(v)) = at;
        if !seen.insert(v) {
            continue;
        }
        let n = f.producer(v);
        match (at, f.node_kind(n)) {
            (At::Value(_), NodeKind::Phi) => {
                work.extend(f.node_inputs(n).into_iter().skip(1).map(At::Value));
            }
            (At::Value(_), NodeKind::Load(space)) if at_slot(space, f.load_addr(n)) => {
                work.push(At::Memory(f.node_inputs(n)[0]));
            }
            (At::Value(_), _) if f.int_const_u128(v).is_some() => {
                values.extend(f.int_const_u128(v))
            }
            (At::Memory(_), NodeKind::Store(space)) if at_slot(space, f.store_addr(n)) => {
                work.push(At::Value(f.store_data(n)));
            }
            (At::Memory(_), NodeKind::Store(_)) => work.push(At::Memory(f.node_inputs(n)[0])),
            (At::Memory(_), NodeKind::MemPhi) => {
                work.extend(f.node_inputs(n).into_iter().skip(1).map(At::Memory));
            }
            (_, kind) => others.push(format!("{kind:?}")),
        }
    }
    assert!(
        values == [7] && others.is_empty(),
        "r0 at the return is {values:x?} or {others:?}\n{}",
        f.to_text(false),
    );
}

#[test]
fn a_named_register_write_survives_an_opaque_register_store_on_arm() {
    r0_is_seven_unless_the_opaque_store_names_it(SleighArch::arm(), false);
}

#[test]
fn a_named_register_write_survives_an_opaque_register_store_on_big_endian_arm() {
    r0_is_seven_unless_the_opaque_store_names_it(SleighArch::arm_be(), true);
}

#[test]
fn a_named_register_write_survives_an_opaque_register_store_on_be8() {
    r0_is_seven_unless_the_opaque_store_names_it(SleighArch::arm_be_kernel(), false);
}
