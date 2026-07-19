//! These use [`with_test_lifter_cc`] rather than the default harness, whose
//! injected `empty_cc` stack_vn would pollute the clobber lists.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::{Vn, VnSpace};

use super::handler_tests::with_test_lifter_cc;

fn reg(off: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: off,
        addr_space: VnSpace::REGISTER,
    }
}

/// Every other field takes its trivial default.  Struct-literal construction
/// skips ABI-disjointness validation, fine for a synthetic fixture.
fn make_cc(
    ret_val_regs: Vec<Vn>,
    callee_saved_regs: Vec<Vn>,
    stack_vn: Vn,
) -> strider_target::BuiltCallingConvention {
    strider_target::BuiltCallingConvention {
        arg_passing_regs: Vec::new(),
        callee_saved_regs,
        ret_val_regs,
        ret_val_regs_float: Vec::new(),
        stack_vn,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        no_return: false,
    }
}

/// A sub-register ret reg (`eax`) must resolve to its container (`rax`) in
/// BOTH the ret-val list and the clobber exclusion.
#[test]
fn sub_register_ret_reg_routes_to_container() {
    let rax = reg(0x0, 8);
    let eax = reg(0x0, 4); // sub-register of rax
    let sp = reg(0x7000, 8);
    let all_vns = vec![rax, sp];
    let cc = make_cc(vec![eax], Vec::new(), sp);
    with_test_lifter_cc(cc.clone(), all_vns, |d, _rid| {
        assert_eq!(
            d.call_ret_vals_for(&cc),
            vec![rax],
            "eax ret reg must resolve to its tracked rax container"
        );
        assert!(
            !d.call_clobbered_for(&cc).contains(&rax),
            "rax is the ret container and must not appear in the clobber list"
        );
    });
}

#[test]
fn ret_and_clobber_split_full_width() {
    let rax = reg(0x00, 8);
    let rcx = reg(0x08, 8);
    let rbx = reg(0x10, 8);
    let sp = reg(0x18, 8);
    let all_vns = vec![rax, rcx, rbx, sp];
    let cc = make_cc(vec![rax], vec![rbx], sp);
    with_test_lifter_cc(cc.clone(), all_vns, |d, _rid| {
        assert_eq!(d.call_ret_vals_for(&cc), vec![rax], "rax is the ret reg");
        let clobbered = d.call_clobbered_for(&cc);
        assert!(!clobbered.contains(&rax), "rax is ret, not a clobber");
        assert!(clobbered.contains(&rcx), "rcx is caller-saved, a clobber");
        assert!(!clobbered.contains(&rbx), "rbx is callee-saved, excluded");
        assert!(!clobbered.contains(&sp), "sp is the stack_vn, excluded");
        assert_eq!(clobbered, vec![rcx], "rcx is the sole clobber");
    });
}

/// Fewer ret regs and more callee-saved must shrink the combined set.
#[test]
fn override_cc_yields_smaller_clobber_set() {
    let r0 = reg(0x10, 8);
    let r1 = reg(0x20, 8);
    let r2 = reg(0x30, 8);
    let sp = reg(0x40, 8);
    let all_vns = vec![r0, r1, r2, sp];

    // cc_A: ret=[r0], callee-saved=[r2], so clobber=[r1].
    let cc_a = make_cc(vec![r0], vec![r2], sp);
    // cc_B: ret=[], callee-saved=[r1, r2], so clobber=[r0].
    let cc_b = make_cc(Vec::new(), vec![r1, r2], sp);

    with_test_lifter_cc(cc_a.clone(), all_vns, |d, _rid| {
        assert_eq!(d.call_ret_vals_for(&cc_a), vec![r0]);
        assert_eq!(d.call_clobbered_for(&cc_a), vec![r1]);

        assert_eq!(d.call_ret_vals_for(&cc_b), Vec::<Vn>::new());
        assert_eq!(d.call_clobbered_for(&cc_b), vec![r0]);

        let combined_a = d.call_ret_vals_for(&cc_a).len() + d.call_clobbered_for(&cc_a).len();
        let combined_b = d.call_ret_vals_for(&cc_b).len() + d.call_clobbered_for(&cc_b).len();
        assert!(
            combined_b < combined_a,
            "override cc_B combined ret+clobber ({combined_b}) must be smaller than cc_A ({combined_a})"
        );
    });
}
