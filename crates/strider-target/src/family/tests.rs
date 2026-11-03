//! Every register name a family carries resolves on every preset it applies to.

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
