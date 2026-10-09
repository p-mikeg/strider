//! The PowerPC AltiVec/VSX vector ABI: `v2` returns a vector, `v2`-`v13`
//! pass them, and `v20`-`v31` are non-volatile.  The sla overlays AltiVec
//! onto the top 32 VSX registers, so `vN` is `vs(32+N)`.

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

/// ELFv2 2.2.3 returns a vector, an IEEE `__float128` and (under
/// `-mabi=ieeelongdouble`) a `long double` in `v2`; the AltiVec supplement
/// says the same for ELFv1 and 32-bit SysV.  Unlisted, nothing roots the
/// vector cone at `Return` and DCE deletes the whole computation.
#[test]
fn every_ppc_preset_returns_a_vector_in_v2() {
    for (name, arch, cc) in ppc_presets() {
        let regs = regs_for(arch);
        let built = cc.build(&regs).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let v2 = regs.name_to_vn("vs34").expect("vs34");
        assert!(
            built.ret_val_regs_float.contains(&v2),
            "{name}: v2 must be a vector return register",
        );
    }
}

/// ELFv2 2.2.3.3 returns a homogeneous vector aggregate of up to eight
/// members in `v2`-`v9`.  ELFv1 and 32-bit SysV return such an aggregate
/// through a hidden pointer, so `v2` alone is the whole story there.
#[test]
fn elfv2_returns_a_homogeneous_vector_aggregate_in_v2_v9() {
    let regs = regs_for(SleighArch::ppc64le());
    let built = CallingConvention::powerpc64_elf_v2()
        .build(&regs)
        .expect("build");
    for n in 34..42u32 {
        let vn = regs.name_to_vn(&format!("vs{n}")).expect("vs name");
        assert!(
            built.ret_val_regs_float.contains(&vn),
            "vs{n} (v{}) must be a vector return register",
            n - 32,
        );
    }
}

/// Vector arguments go in `v2`-`v13` on all three ABIs, after the
/// floating-point ones so the existing float positions do not shift.
#[test]
fn every_ppc_preset_passes_vectors_in_v2_v13() {
    for (name, arch, cc) in ppc_presets() {
        let regs = regs_for(arch);
        let built = cc.build(&regs).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let want: Vec<rsleigh::Vn> = (34..46)
            .map(|n| regs.name_to_vn(&format!("vs{n}")).expect("vs name"))
            .collect();
        let floats = built.arg_passing_regs_float.len() - want.len();
        assert_eq!(
            &built.arg_passing_regs_float[floats..],
            &want[..],
            "{name}: v2..v13 follow the float argument registers",
        );
    }
}

/// ELFv2 2.2.1 and the AltiVec supplement make `v20`-`v31` non-volatile in
/// full.
#[test]
fn every_ppc_preset_preserves_v20_v31() {
    for (name, arch, cc) in ppc_presets() {
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
