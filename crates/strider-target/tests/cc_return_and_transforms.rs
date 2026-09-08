//! Float-return coverage for the x87 stack, and the invariants the CC
//! transforms and `validate` must hold.

use strider_target::{CallingConvention, SleighArch};

fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("Sleigh::new");
    sleigh.regs().expect("Sleigh::regs")
}

fn float_ret_names(cc: CallingConvention, arch: SleighArch) -> Vec<String> {
    let regs = regs_for(arch);
    cc.build(&regs)
        .expect("build")
        .ret_val_regs_float
        .iter()
        .map(|vn| {
            regs.vn_to_name(*vn)
                .expect("every resolved float-ret vn must round-trip to a name")
                .to_string()
        })
        .collect()
}

/// SysV AMD64 psABI 3.2.3: a COMPLEX_X87 return puts the real part in `%st0`
/// and the imaginary part in `%st1`. Without ST1 in the list nothing roots the
/// imaginary half's cone and DCE deletes it.
#[test]
fn x86_64_returns_complex_long_double_imaginary_half_in_st1() {
    let names = float_ret_names(CallingConvention::x86_64_systemv(), SleighArch::x86_64());
    assert!(
        names.iter().any(|n| n == "ST1"),
        "ST1 must be a float return register, got {names:?}"
    );
}

/// Same rule in the Intel386 psABI.
#[test]
fn x86_returns_complex_long_double_imaginary_half_in_st1() {
    let names = float_ret_names(CallingConvention::x86_cdecl(), SleighArch::x86());
    assert!(
        names.iter().any(|n| n == "ST1"),
        "ST1 must be a float return register, got {names:?}"
    );
}

/// `preserves_regs` clobbers memory, and the load-forwarding gate is
/// escape-based: dropping the argument registers hides a frame address handed
/// to the callee, so a spill wrongly forwards across the call.
#[test]
fn preserves_regs_keeps_the_argument_registers() {
    let regs = regs_for(SleighArch::x86_64());
    let base = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build");
    let preserved = CallingConvention::x86_64_systemv()
        .preserves_regs()
        .build(&regs)
        .expect("build");
    assert!(
        !base.arg_passing_regs.is_empty(),
        "baseline must pass arguments in registers"
    );
    assert_eq!(
        preserved.arg_passing_regs, base.arg_passing_regs,
        "preserves_regs must keep the argument registers: memory is still clobbered, \
         so frame-escape evidence has to survive"
    );
}

/// `preserves_all` preserves memory too, so every escape is covered.
#[test]
fn preserves_all_still_drops_the_argument_registers() {
    let regs = regs_for(SleighArch::x86_64());
    let cc = CallingConvention::x86_64_systemv()
        .preserves_all()
        .build(&regs)
        .expect("build");
    assert!(cc.arg_passing_regs.is_empty());
    assert!(cc.preserves_memory);
}

/// `is_clobbered` short-circuits on `preserves_all_registers`, so it silently
/// wins over a populated return list and the `Call` emits no ret-val output.
#[test]
fn validate_rejects_preserve_all_alongside_return_registers() {
    let regs = regs_for(SleighArch::x86_64());
    let mut cc = CallingConvention::x86_64_systemv()
        .build(&regs)
        .expect("build");
    assert!(!cc.ret_val_regs.is_empty(), "baseline returns in registers");
    cc.preserves_all_registers = true;
    let err = cc
        .validate()
        .expect_err("preserves_all_registers with a populated ret list is contradictory");
    let msg = err.to_string();
    assert!(
        msg.contains("preserves_all_registers"),
        "error must name the offending field, got {msg:?}"
    );
}

/// A MIPS o32 `double` return lives in the $f0/$f1 pair.  `ret_val_regs()`
/// hands the CC list to the lifter with no container projection, so a 4-byte
/// `f0` here truncates the return to I32; the pair register is 8 bytes.
#[test]
fn mips_o32_double_return_covers_the_full_fpr_pair() {
    for arch in [SleighArch::mipsbe32(), SleighArch::mipsle32()] {
        let regs = regs_for(arch);
        let built = CallingConvention::mips_o32().build(&regs).expect("build");
        assert_eq!(
            float_ret_names(CallingConvention::mips_o32(), arch),
            vec!["f0_1".to_string(), "f2_3".to_string()],
        );
        for vn in &built.ret_val_regs_float {
            assert_eq!(vn.size, 8, "{arch:?}: a double return needs all 8 bytes");
        }
    }
}

/// n64's FPRs are already 8 bytes and its sla declares no pair register, so
/// o32's pair naming must not leak into it.
#[test]
fn mips_n64_double_return_uses_the_plain_eight_byte_fprs() {
    for arch in [SleighArch::mipsbe64(), SleighArch::mipsle64()] {
        let regs = regs_for(arch);
        let built = CallingConvention::mips_n64().build(&regs).expect("build");
        assert_eq!(
            float_ret_names(CallingConvention::mips_n64(), arch),
            vec!["f0".to_string(), "f2".to_string()],
        );
        for vn in &built.ret_val_regs_float {
            assert_eq!(vn.size, 8, "{arch:?}");
        }
    }
}

/// The o32 float ARGUMENT side needs no such renaming: `float_arg_slots`
/// projects `f12` / `f14` through the container map, and they land in the
/// DISTINCT `f12_13` / `f14_15` pairs, so neither slot is shared and both
/// carry the full 8 bytes.
#[test]
fn mips_o32_double_arguments_project_to_distinct_full_width_pairs() {
    for arch in [SleighArch::mipsbe32(), SleighArch::mipsle32()] {
        let regs = regs_for(arch);
        let built = CallingConvention::mips_o32().build(&regs).expect("build");
        let pair = |name: &str| regs.name_to_vn(name).expect(name);
        let (f12_13, f14_15) = (pair("f12_13"), pair("f14_15"));
        assert_ne!(f12_13, f14_15, "{arch:?}: the two pairs must be distinct");
        let container_of = |v: &rsleigh::Vn| {
            for c in [f12_13, f14_15] {
                if c.addr_space == v.addr_space
                    && c.addr_off <= v.addr_off
                    && u128::from(c.addr_off) + u128::from(c.size)
                        >= u128::from(v.addr_off) + u128::from(v.size)
                {
                    return c;
                }
            }
            *v
        };
        let slots = built.float_arg_slots(&[f12_13, f14_15], container_of);
        let names: Vec<String> = slots
            .iter()
            .map(|s| {
                let vn = s.expect("every o32 float arg slot has a carrier");
                assert_eq!(vn.size, 8, "{arch:?}: a double argument needs 8 bytes");
                regs.vn_to_name(vn).expect("named").to_string()
            })
            .collect();
        assert_eq!(names, vec!["f12_13".to_string(), "f14_15".to_string()]);
    }
}
