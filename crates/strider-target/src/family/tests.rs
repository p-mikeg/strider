//! Family tables checked against the presets they apply to.

use crate::ArchPreset;
use crate::call_other_abi::CallOtherClass;

/// A name that does not resolve is dropped or is a hard error at lift time,
/// so a typo in any family table is otherwise invisible until a binary hits it.
#[test]
fn every_family_register_name_resolves_on_every_preset_it_applies_to() {
    let mut checked = 0usize;
    for &preset in ArchPreset::ALL {
        let family = preset.family();
        let regs = preset.arch().probe_regs();
        let mut check = |what: &str, name: &str| {
            assert!(
                regs.name_to_vn(name).is_some(),
                "{preset:?}: {what} names `{name}`, which the sla does not declare",
            );
            checked += 1;
        };

        let facts = &family.facts;
        for (what, names) in [
            ("internal_registers", facts.internal_registers),
            ("volatile_registers", facts.volatile_registers),
            ("non_volatile_registers", facts.non_volatile_registers),
        ] {
            for name in names {
                check(what, name);
            }
        }

        for row in family.call_other {
            if !row.only.is_none_or(|only| only.contains(&preset)) {
                continue;
            }
            let CallOtherClass::Call(abi) = row.class else {
                continue;
            };
            for name in abi.implicit_reads.iter().chain(abi.implicit_writes) {
                check(&format!("call_other row {:?}", row.names), name);
            }
        }

        for cc in family
            .calling_conventions
            .iter()
            .filter(|cc| cc.arches.contains(&preset))
        {
            let c = &cc.cc;
            let lists = [
                c.arg_passing_regs,
                c.arg_passing_regs_float,
                c.callee_saved_regs,
                c.ret_val_regs,
                c.ret_val_regs_float,
            ];
            let what = format!("calling convention {}", cc.name);
            for name in lists.into_iter().flatten().copied() {
                check(&what, name);
            }
            for name in std::iter::once(c.stack_ptr)
                .chain(c.link_register)
                .chain(c.entry_address)
            {
                check(&what, name);
            }
        }
    }
    assert!(checked > 1000, "only {checked} names checked");
}

/// Every preset a family's rows name belongs to that family, so a row cannot
/// be scoped to a preset whose `family()` never asks it.
#[test]
fn every_family_row_names_only_its_own_presets() {
    for family in super::FAMILIES {
        let named = family
            .call_other
            .iter()
            .flat_map(|row| row.only.unwrap_or_default())
            .chain(family.calling_conventions.iter().flat_map(|cc| cc.arches));
        for preset in named {
            assert!(
                preset.family().name == family.name,
                "{}: names {preset:?}, a {} preset",
                family.name,
                preset.family_name(),
            );
        }
    }
}

/// The families of `ArchPreset::ALL` are exactly `FAMILIES`, each once.
#[test]
fn every_preset_family_is_registered_once() {
    let mut from_presets: Vec<&str> = ArchPreset::ALL.iter().map(|p| p.family_name()).collect();
    from_presets.sort_unstable();
    from_presets.dedup();
    let mut registered: Vec<&str> = super::FAMILIES.iter().map(|f| f.name).collect();
    registered.sort_unstable();
    let len = registered.len();
    registered.dedup();
    assert_eq!(registered.len(), len, "a family is registered twice");
    assert_eq!(from_presets, registered);
}

/// The ARM32 register-space reach covers the d-register file and leaves the
/// core registers out.
#[test]
fn arm32_register_space_reach_is_the_neon_file() {
    for preset in [
        ArchPreset::Arm,
        ArchPreset::ArmBe,
        ArchPreset::ArmBeKernel,
        ArchPreset::ArmThumb,
    ] {
        let arch = preset.arch();
        let reach = arch.register_space_reach().expect("reach");
        let regs = arch.probe_regs();
        for d in 0..32 {
            let vn = regs.name_to_vn(&format!("d{d}")).expect("d register");
            let end = vn.addr_off + u64::from(vn.size);
            assert!(
                reach.start <= vn.addr_off && end <= reach.end,
                "{preset:?}: d{d}"
            );
        }
        for name in ["r0", "sp", "lr", "pc", "cpsr", "fpscr"] {
            let vn = regs.name_to_vn(name).expect(name);
            assert!(!reach.contains(&vn.addr_off), "{preset:?}: {name}");
        }
    }
}
