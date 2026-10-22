//! `fixtures/loader/out/ppc32be_got2.elf`: a PPC32 `-fPIC` library whose
//! `f` and `g` load their jump-table bases out of `.got2`, the per-object
//! GOT, after `bcl 20,31,$+4; mflr r30`:
//!
//! ```text
//! 2a0: lwz    r10,-32768(r30)
//! 2a4: rlwinm r9,r9,2,0,29
//! 2a8: lwzx   r9,r10,r9
//! 2ac: add    r9,r9,r10
//! 2b0: mtctr  r9
//! 2b4: bctr
//! ```
//!
//! Each `.got2` slot is one `R_PPC_RELATIVE`, so the ROM that serves the
//! relocated GOT serves it too.

use object::{Object as _, ObjectSection as _};
use strider_orchestrator::{AnalyzeOptions, Reason};

mod common;
use common::Arch;
use common::fixtures::{Fixture, fixtures_root};

fn fixture() -> Fixture {
    Fixture::at(
        Arch::Ppc32be.fixture(),
        fixtures_root().join("loader/out/ppc32be_got2.elf"),
    )
}

/// Each table's targets, decoded from the image: the `.got2` relocations
/// name the table bases, each table runs to the next base or the end of
/// `.rodata`, and a target is the base plus a signed big-endian word.
fn tables(fx: &Fixture) -> Vec<Vec<u64>> {
    let obj = fx.file();
    let got2 = obj.section_by_name(".got2").expect(".got2");
    let got2 = got2.address()..got2.address() + got2.size();
    let mut bases: Vec<u64> = obj
        .dynamic_relocations()
        .expect("dynamic relocations")
        .filter(|(site, _)| got2.contains(site))
        .map(|(_, r)| r.addend().cast_unsigned())
        .collect();
    bases.sort_unstable();
    let rodata = obj.section_by_name(".rodata").expect(".rodata");
    let data = rodata.data().expect("data");
    let end = rodata.address() + rodata.size();
    let mut out = Vec::new();
    for (i, &base) in bases.iter().enumerate() {
        let stop = bases.get(i + 1).copied().unwrap_or(end);
        let mut arms: Vec<u64> = (base..stop)
            .step_by(4)
            .map(|at| {
                let off = usize::try_from(at - rodata.address()).unwrap();
                let word = i32::from_be_bytes(data[off..off + 4].try_into().unwrap());
                base.wrapping_add_signed(i64::from(word))
            })
            .collect();
        arms.sort_unstable();
        arms.dedup();
        out.push(arms);
    }
    out
}

/// The arms of every `Switch` the analysis of `name` seats, and the sites it
/// leaves unresolved.
fn analyze(fx: &Fixture, serve_got: bool, name: &str) -> (Vec<Vec<u64>>, Vec<u64>) {
    let (mut strider, cc) = if serve_got {
        fx.strider_serving_got()
    } else {
        fx.strider()
    };
    let result = strider
        .analyze(fx.symbol(name), &cc, &AnalyzeOptions::default(), None)
        .expect("analyze");
    let unresolved = result
        .incomplete()
        .of(Reason::UnresolvedBranch)
        .map(|a| a.machine_addr.addr)
        .collect();
    let switches = result
        .cfg
        .regions()
        .filter_map(|region| match &region.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => {
                let mut arms: Vec<u64> = targets.iter().map(|t| t.addr).collect();
                arms.sort_unstable();
                arms.dedup();
                Some(arms)
            }
            _ => None,
        })
        .collect();
    (switches, unresolved)
}

/// Without the GOT served, the table base is unknown and both sites defer.
#[test]
fn a_got2_table_base_left_unserved_defers() {
    let fx = fixture();
    for name in ["f", "g"] {
        let (switches, unresolved) = analyze(&fx, false, name);
        assert!(switches.is_empty(), "{name}: {switches:#x?}");
        assert_eq!(unresolved.len(), 1, "{name}");
    }
}

/// Served, each site resolves to exactly its table's targets.
/// Mutation: drop `.got2` from the relocated-GOT section names.
#[test]
fn a_got2_table_base_served_resolves_to_the_tables_targets() {
    let fx = fixture();
    let tables = tables(&fx);
    assert_eq!(
        tables.iter().map(Vec::len).collect::<Vec<_>>(),
        [9, 8],
        "{tables:#x?}"
    );
    for (name, table) in ["f", "g"].into_iter().zip(&tables) {
        let (switches, unresolved) = analyze(&fx, true, name);
        assert!(unresolved.is_empty(), "{name}: {unresolved:#x?}");
        assert_eq!(switches, std::slice::from_ref(table), "{name}");
    }
}
