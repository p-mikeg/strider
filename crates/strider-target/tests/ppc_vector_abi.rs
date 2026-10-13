//! The PowerPC vector register FILE: the sla overlays AltiVec onto the top 32
//! VSX registers, so `vN` is `vs(32+N)` and the ABI's `v2`-`v13` argument and
//! `v20`-`v31` non-volatile windows are byte-disjoint from `f0`-`f31`.  Which
//! registers each convention names is in the preset case table.

use strider_target::{CallingConvention, SleighArch};

fn regs_for(arch: SleighArch) -> rsleigh::SleighRegs {
    arch.probe_regs().expect("probe_regs")
}

/// `(arch, cc)` for every PowerPC preset, ELFv1 included (which no
/// `SleighArch` picks on its own).
fn ppc_presets() -> Vec<(&'static str, SleighArch, CallingConvention)> {
    vec![
        (
            "sysv32 BE",
            SleighArch::ppc32be(),
            CallingConvention::powerpc_sysv32(),
        ),
        (
            "sysv32 LE",
            SleighArch::ppc32le(),
            CallingConvention::powerpc_sysv32(),
        ),
        (
            "ELFv1",
            SleighArch::ppc64be(),
            CallingConvention::powerpc64_elf_v1(),
        ),
        (
            "ELFv2",
            SleighArch::ppc64le(),
            CallingConvention::powerpc64_elf_v2(),
        ),
    ]
}

/// `(arch, cc)` for the kernel flavour of each preset above, in the same
/// order.
fn ppc_kernel_presets() -> Vec<(&'static str, SleighArch, CallingConvention)> {
    vec![
        (
            "sysv32 kernel BE",
            SleighArch::ppc32be(),
            CallingConvention::powerpc_sysv32_linux_kernel(),
        ),
        (
            "sysv32 kernel LE",
            SleighArch::ppc32le(),
            CallingConvention::powerpc_sysv32_linux_kernel(),
        ),
        (
            "ELFv1 kernel",
            SleighArch::ppc64be(),
            CallingConvention::powerpc64_elf_v1_linux_kernel(),
        ),
        (
            "ELFv2 kernel",
            SleighArch::ppc64le(),
            CallingConvention::powerpc64_elf_v2_linux_kernel(),
        ),
    ]
}

/// A userland preset names the AltiVec argument and return windows; the
/// kernel one, for code built `-mno-altivec -mno-vsx`, names neither, so
/// `FunctionBuilder::new` seeds no 16-byte varnode on a function that never
/// touches a vector.
#[test]
fn only_the_userland_presets_name_a_vector_argument_or_return() {
    let is_vector = |vn: &rsleigh::Vn| vn.size == 16;
    for (name, arch, cc) in ppc_presets() {
        let built = cc
            .build(&regs_for(arch))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(
            built.arg_passing_regs_float.iter().any(is_vector),
            "{name}: no vector argument register",
        );
        assert!(
            built.ret_val_regs_float.iter().any(is_vector),
            "{name}: no vector return register",
        );
    }
    for (name, arch, cc) in ppc_kernel_presets() {
        let built = cc
            .build(&regs_for(arch))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        built.validate().unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(
            !built.arg_passing_regs_float.iter().any(is_vector),
            "{name}: {:?} names a vector argument register",
            built.arg_passing_regs_float,
        );
        assert!(
            !built.ret_val_regs_float.iter().any(is_vector),
            "{name}: {:?} names a vector return register",
            built.ret_val_regs_float,
        );
    }
}

/// The two flavours differ in the vector windows alone: dropping them must
/// move no float or integer position, which would renumber `arg(n)`.
#[test]
fn the_kernel_presets_keep_every_non_vector_position() {
    for ((name, arch, user), (_, _, kernel)) in ppc_presets().into_iter().zip(ppc_kernel_presets())
    {
        let regs = regs_for(arch);
        let user = user
            .build(&regs)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let kernel = kernel
            .build(&regs)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let scalars = |vns: &[rsleigh::Vn]| -> Vec<rsleigh::Vn> {
            vns.iter().copied().filter(|vn| vn.size != 16).collect()
        };
        assert_eq!(kernel.arg_passing_regs, user.arg_passing_regs, "{name}");
        assert_eq!(
            kernel.arg_passing_regs_float,
            scalars(&user.arg_passing_regs_float),
            "{name}: float argument positions",
        );
        assert_eq!(kernel.ret_val_regs, user.ret_val_regs, "{name}");
        assert_eq!(
            kernel.ret_val_regs_float,
            scalars(&user.ret_val_regs_float),
            "{name}: float return positions",
        );
        assert_eq!(kernel.callee_saved_regs, user.callee_saved_regs, "{name}");
        assert_eq!(kernel.stack_args, user.stack_args, "{name}");
    }
}

/// `vsN` is 16 bytes at `0x4000 + 16N`, and the float file overlays only
/// `vs0`-`vs31`, so every vector register the ABI names (`v2` = `vs34`
/// upward) is byte-disjoint from `f0`-`f31`.  The two banks sharing a byte
/// would make one argument out of a float and a vector.
#[test]
fn the_vector_bank_is_byte_disjoint_from_the_float_bank() {
    for (name, arch, _) in ppc_presets() {
        let regs = regs_for(arch);
        let vn = |n: &str| regs.name_to_vn(n).unwrap_or_else(|| panic!("{name}: {n}"));
        for n in 32..64u64 {
            let v = vn(&format!("vs{n}"));
            assert_eq!(
                (v.addr_off, v.size),
                (0x4000 + n * 16, 16),
                "{name}: vs{n} moved"
            );
            for f in 0..32u32 {
                let f = vn(&format!("f{f}"));
                assert!(
                    f.addr_off + u64::from(f.size) <= v.addr_off
                        || v.addr_off + u64::from(v.size) <= f.addr_off,
                    "{name}: vs{n} shares bytes with {f:?}",
                );
            }
        }
    }
}

/// ELFv2 2.2.1 and the AltiVec supplement make `v20`-`v31` non-volatile in
/// full.  Preserving a register the code never writes costs nothing, so the
/// kernel flavour keeps the whole non-volatile window.
#[test]
fn every_ppc_preset_preserves_v20_v31() {
    for (name, arch, cc) in ppc_presets().into_iter().chain(ppc_kernel_presets()) {
        let regs = regs_for(arch);
        let built = cc.build(&regs).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        for n in 52..=63u32 {
            let vn = regs.name_to_vn(&format!("vs{n}")).expect("vs name");
            assert!(
                built.callee_saved_regs.contains(&vn),
                "{name}: vs{n} (v{}) must be callee-saved",
                n - 32,
            );
        }
    }
}
