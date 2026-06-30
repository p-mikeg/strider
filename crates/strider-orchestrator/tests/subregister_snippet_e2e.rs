//! Sub-register aliasing end-to-end: a snippet that writes a wide
//! register (`rax`) and then reads/writes a narrow alias (`al`) must
//! lift through the container-register dispatch (`read_reg_vn` /
//! `write_reg_vn`) — the raw IR carries the truncate/mask shape — and
//! the optimiser must fold the whole chain to the right constant in
//! the ret-val register.
//!
//! Snippet (x86_64):
//!
//! ```text
//!   1000:  48 c7 c0 ff 00 00 00   mov rax, 0xff
//!   1007:  04 01                  add al, 1     ; al = 0xff+1 wraps to 0x00
//!   1009:  c3                     ret
//! ```
//!
//! Hand-computed result: `rax = 0xff`, then `al = (0xff + 1) & 0xff =
//! 0x00` written back into bits 0-7 of the container while bits 8-63
//! (all zero) are preserved → `rax == 0` at `ret`.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod common;

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::MachineInsnAddr;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x1000;

fn snippet_bytes() -> Vec<u8> {
    vec![
        0x48, 0xc7, 0xc0, 0xff, 0x00, 0x00, 0x00, // mov rax, 0xff
        0x04, 0x01, // add al, 1
        0xc3, // ret
    ]
}

#[test]
fn narrow_alias_read_lifts_with_truncate_or_mask_shape() {
    // Lift WITHOUT the optimiser: the `al` read of the tracked `rax`
    // container must insert a Truncate (or an And mask) — that shape is
    // the register-aliasing dispatch's signature.
    let reader = BufMemReader::new(snippet_bytes(), BASE);
    let (mut driver, cc) = common::strider_x86_64(reader);
    let cfg = driver
        .build_cfg(
            MachineInsnAddr::from(BASE),
            &strider_cfg::CfgOptions::default(),
        )
        .expect("cfg");
    let function = driver.build_ir(&cfg, &cc).expect("build_ir").function;

    let truncates = common::count_kind(&function, |k| matches!(k, NodeKind::Truncate));
    let ands = common::count_int_binop(&function, strider_ir::IntBinaryOp::And);
    assert!(
        truncates + ands >= 1,
        "raw lift of an `al` alias read must contain a Truncate or And mask; \
         got {truncates} Truncate and {ands} And nodes"
    );
}

#[test]
fn narrow_alias_write_folds_to_zero_in_ret_val_register() {
    // Full orchestrator run: the optimiser folds the container
    // read-modify-write chain; the Return's rax slot must be
    // IntConst(0) (al wrapped 0xff+1 → 0x00, upper bits already zero).
    let arch = strider_target::SleighArch::x86_64();
    let reader = BufMemReader::new(snippet_bytes(), BASE);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let regs = sleigh.regs().expect("regs");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    let result = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    assert!(result.unresolved_indirect_branches.is_empty());
    let function = result.function;

    // Locate the unique Return and inspect its ret-val inputs
    // (slots 2.. after [control, memory]).
    let ret = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
        .expect("snippet must lift to a Return");
    let inputs: Vec<_> = function.node_inputs(ret).into_iter().collect();
    assert!(
        inputs.len() > 2,
        "SystemV Return must carry ret-val inputs; got {} inputs",
        inputs.len()
    );
    let ret_vals: Vec<Option<u128>> = inputs[2..]
        .iter()
        .map(|&v| function.int_const_u128(v))
        .collect();
    // Pinned: the optimiser fully folds the chain — the first ret-val
    // slot (rax in the SystemV ret-val order) is IntConst(0); the
    // remaining ret-val slots (rdx / float regs) stay non-constant
    // function-entry values.
    assert_eq!(
        ret_vals.first().copied().flatten(),
        Some(0),
        "the rax ret-val slot must fold to IntConst(0); got {ret_vals:?}"
    );
}
