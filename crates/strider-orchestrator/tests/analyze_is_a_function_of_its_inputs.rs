//! `Strider::analyze` on a reused engine answers what a fresh one answers.
//!
//! The resolve loop converges only if each round's cfg is a function of
//! `known_targets`, yet `Lifter::build_cfg` pins a function's first round from
//! the pspec defaults and every later one from its `entry_contexts` memo, and
//! the engine keeps every context commit a prior function or round made. Every
//! fixture function is analysed on a fresh `Strider`, then twice more on one
//! shared across the whole image, and the three cfgs and reports must agree.
//!
//! ```text
//! cargo test --release -p strider-orchestrator --test analyze_is_a_function_of_its_inputs -- --ignored --nocapture
//! ```

mod common;

use common::{ALL_ARCHES, Arch};
use object::{Object, ObjectSymbol, SymbolKind};
use strider_orchestrator::Strider;
use strider_reader::ElfFileMemReader;

fn strider_for(arch: Arch, obj: &object::File<'_>) -> Strider<ElfFileMemReader> {
    let sleigh_arch = arch.sleigh();
    let mem = ElfFileMemReader::from_object(obj).expect("mem reader");
    let rom = ElfFileMemReader::from_object(obj).expect("rom reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("create sleigh");
    Strider::new(sleigh_arch, sleigh, Some(Box::new(rom))).expect("Strider::new")
}

fn fingerprint(
    strider: &mut Strider<ElfFileMemReader>,
    cc: &strider_target::BuiltCallingConvention,
    entry: u64,
) -> String {
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            allow_code_before_start_addr: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let result = match strider.analyze(entry, cc, &lift_opts, &Default::default(), None) {
        Ok(result) => result,
        Err(e) => return format!("error: {e:#}"),
    };
    let cfg = &result.cfg;
    let mut regions: Vec<String> = cfg
        .region_ids()
        .map(|r| {
            let region = &cfg.region_graph()[r];
            let mut successors: Vec<u64> = cfg
                .region_graph()
                .neighbors(r)
                .map(|s| cfg.region_graph()[s].start_addr.machine_addr.addr)
                .collect();
            successors.sort_unstable();
            let insns: Vec<(u64, u64, u32)> = region
                .insns
                .iter()
                .map(|i| (i.addr.machine_addr.addr, i.addr.insn_index, i.len))
                .collect();
            format!(
                "{:?} {insns:?} {:?} -> {successors:x?}",
                region.start_addr, region.terminator
            )
        })
        .collect();
    regions.sort_unstable();
    format!(
        "{regions:#?}\nunresolved {:?}\nunverified {:?}\nisa {:?}\ninterior {:?}\nunmapped {:?}",
        result.unresolved_indirect_branches,
        result.unverified_seeded_sites,
        result.isa_mode_conflicts,
        result.interior_branch_targets,
        result.unmapped_branch_targets,
    )
}

#[test]
#[ignore = "broad fixture sweep; three analyses of every fixture function"]
fn a_reused_strider_analyses_every_fixture_function_as_a_fresh_one_does() {
    let mut total = 0usize;
    let mut failures = Vec::new();
    for &arch in ALL_ARCHES {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/out")
            .join(arch.name());
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("elf"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(owned) = strider_reader::load_elf(&path) else {
                continue;
            };
            let obj = owned.checked_file().expect("the mapped file is unchanged");
            let mut addrs: Vec<u64> = obj
                .symbols()
                .filter(|s| s.kind() == SymbolKind::Text && s.size() > 0)
                .map(|s| s.address())
                .collect();
            addrs.sort_unstable();
            addrs.dedup();
            let mut shared = strider_for(arch, &obj);
            let cc = arch.cc().build(shared.sleigh_regs()).expect("cc");
            let fresh: Vec<String> = addrs
                .iter()
                .map(|&a| fingerprint(&mut strider_for(arch, &obj), &cc, a))
                .collect();
            for pass in 1..=2 {
                for (&addr, want) in addrs.iter().zip(&fresh) {
                    let got = fingerprint(&mut shared, &cc, addr);
                    if &got != want {
                        failures.push(format!(
                            "{} {addr:#x} pass {pass}:\nfresh:\n{want}\nshared:\n{got}",
                            path.display()
                        ));
                    }
                }
            }
            total += addrs.len();
        }
        eprintln!("{}: done, {total} functions so far", arch.name());
    }
    assert!(
        failures.is_empty(),
        "{} of {total} functions differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
