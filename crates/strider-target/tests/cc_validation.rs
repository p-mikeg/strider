use rsleigh::{Vn, VnSpace};
use strider_target::BuiltCallingConvention;

fn vn(off: u64) -> Vn {
    Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: off,
        size: 8,
    }
}

#[test]
fn validate_rejects_sp_in_arg_passing_regs() {
    let sp = vn(0x40);
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![vn(0x10), sp, vn(0x20)],
        callee_saved_regs: vec![],
        ret_val_regs: vec![vn(0x18)],
        ret_val_regs_float: vec![],
        stack_ptr: sp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register: None,
        preserves_memory: false,
        preserves_all_registers: false,
        ..Default::default()
    };
    let res = cc.validate();
    assert!(
        res.is_err(),
        "SP listed in arg_passing_regs must be rejected"
    );
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("stack pointer") && msg.contains("arg_passing_regs"),
        "error must name both the stack pointer and the offending list, got: {msg}",
    );
}

/// A function's own link register is in `callee_saved_regs` so
/// `InitialVar(lr)` propagates through a call site. A per-address callee's
/// may be left out, so the call clobbers it.
#[test]
fn only_a_default_convention_needs_its_link_register_callee_saved() {
    let lr = vn(0x58);
    let cc = BuiltCallingConvention {
        callee_saved_regs: vec![vn(0x20)],
        stack_ptr: vn(0x40),
        link_register: Some(lr),
        ..Default::default()
    };
    cc.validate()
        .expect("a callee may clobber its link register");
    let msg = cc
        .validate_default()
        .expect_err("a link register outside callee_saved_regs must be rejected")
        .to_string();
    assert!(
        msg.contains("link_register") && msg.contains("callee_saved_regs"),
        "error must name both, got: {msg}",
    );
    // Listed, it validates.
    BuiltCallingConvention {
        callee_saved_regs: vec![vn(0x20), lr],
        stack_ptr: vn(0x40),
        link_register: Some(lr),
        ..Default::default()
    }
    .validate_default()
    .expect("a listed link register validates");
}

#[test]
fn validate_rejects_arg_overlapping_callee_saved() {
    let shared = vn(0x10);
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![shared],
        callee_saved_regs: vec![shared],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register: None,
        preserves_memory: false,
        preserves_all_registers: false,
        ..Default::default()
    };
    let res = cc.validate();
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs") && msg.contains("callee_saved_regs"),
        "error should name both lists; got {msg}",
    );
}

#[test]
fn validate_rejects_ret_int_overlapping_ret_float() {
    // Integer and float returns are physically different register files on
    // every supported arch, so the same varnode in both is a CC-author bug.
    // Argument-vs-return overlap stays legal: x86_64 SysV RDX is both.
    let shared = vn(0x28);
    let cc = BuiltCallingConvention {
        arg_passing_regs: vec![vn(0x10)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![shared],
        ret_val_regs_float: vec![shared],
        stack_ptr: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register: None,
        preserves_memory: false,
        preserves_all_registers: false,
        ..Default::default()
    };
    let res = cc.validate();
    assert!(res.is_err());
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("ret_val_regs") && msg.contains("ret_val_regs_float"),
        "error should name both ret lists; got {msg}",
    );
}

#[test]
fn validate_accepts_clean_layout() {
    BuiltCallingConvention {
        arg_passing_regs: vec![vn(0x10), vn(0x18)],
        callee_saved_regs: vec![vn(0x20)],
        ret_val_regs: vec![vn(0x28)],
        ret_val_regs_float: vec![],
        stack_ptr: vn(0x40),
        stack_args: None,
        ret_stack_pop: 0,
        link_register: None,
        preserves_memory: false,
        preserves_all_registers: false,
        ..Default::default()
    }
    .validate()
    .expect("clean layout must validate");
}

/// A wider container holding a narrower one is not a distinct register:
/// AArch64's `q8` occupies the same bytes as `d8` plus eight more, so `d8`
/// callee-saved alongside `q8` argument-passing is the same contradiction as
/// naming one varnode twice.  `preserves_all_bytes_of` is byte-accurate at run time and
/// the doc says "disjoint", so the validator has to be byte-accurate too.
#[test]
fn validate_rejects_container_overlap_not_only_exact_equality() {
    let d8 = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x100,
        size: 8,
    };
    let q8 = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x100,
        size: 16,
    };
    let cc = BuiltCallingConvention {
        arg_passing_regs_float: vec![q8],
        callee_saved_regs: vec![d8],
        stack_ptr: vn(0x40),
        ..Default::default()
    };
    let res = cc.validate();
    assert!(
        res.is_err(),
        "q8 in an argument list overlaps callee-saved d8 and must be rejected"
    );
    let msg = res.unwrap_err().to_string();
    assert!(
        msg.contains("arg_passing_regs_float") && msg.contains("callee_saved_regs"),
        "error must name both lists; got {msg}",
    );
}

/// Overlap is per address space and per byte range: a REGISTER varnode and a
/// same-offset varnode in another space are different storage, and adjacent
/// registers must stay legal or every preset would fail.
#[test]
fn validate_accepts_adjacent_and_cross_space_ranges() {
    let arg = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x100,
        size: 8,
    };
    let adjacent = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x108,
        size: 8,
    };
    let other_space = Vn {
        addr_space: VnSpace::UNIQUE,
        addr_off: 0x100,
        size: 8,
    };
    BuiltCallingConvention {
        arg_passing_regs: vec![arg],
        callee_saved_regs: vec![adjacent, other_space],
        stack_ptr: vn(0x40),
        ..Default::default()
    }
    .validate()
    .expect("adjacent and cross-space ranges do not overlap");
}

/// The `stack_ptr` rule is byte-level like every other disjointness rule
/// here: a SUB-register of the SP in a list is the SP's storage.  An SP
/// sub-register in an argument list makes the stack pointer's tracked
/// container `function_arg(0)`.
#[test]
fn validate_rejects_a_stack_pointer_subregister() {
    let sp = vn(0x40);
    // The 4-byte low half, `ESP` inside `RSP`.
    let sp_low = Vn {
        addr_space: VnSpace::REGISTER,
        addr_off: 0x40,
        size: 4,
    };
    for (list_name, cc) in [
        (
            "callee_saved_regs",
            BuiltCallingConvention {
                callee_saved_regs: vec![sp_low, vn(0x10)],
                stack_ptr: sp,
                ..Default::default()
            },
        ),
        (
            "arg_passing_regs",
            BuiltCallingConvention {
                arg_passing_regs: vec![sp_low, vn(0x10)],
                stack_ptr: sp,
                ..Default::default()
            },
        ),
    ] {
        let msg = cc
            .validate()
            .expect_err("an SP sub-register must be rejected")
            .to_string();
        assert!(
            msg.contains("stack pointer") && msg.contains(list_name),
            "error must name the stack pointer and {list_name}, got: {msg}",
        );
    }
}
