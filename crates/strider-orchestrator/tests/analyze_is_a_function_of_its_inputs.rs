//! `Strider::analyze` on a reused engine answers what a fresh one answers.
//!
//! The resolve loop converges only if each round's cfg is a function of
//! `known_targets`, yet `Lifter::build_cfg` pins a function's first round from
//! the pspec defaults and every later one from its `entry_contexts` memo, and
//! the engine keeps every context commit a prior function or round made. Every
//! fixture function is analysed on a fresh `Strider`, then twice more on one
//! shared across the whole image, and the three cfgs, reports and optimized
//! functions' text must agree.
//!
//! ```text
//! cargo test --release -p strider-orchestrator --test analyze_is_a_function_of_its_inputs -- --ignored --nocapture
//! ```

mod common;

use common::fixtures::{FIXTURE_ARCHES, Fixture};
use strider_orchestrator::{AnalyzeResult, Strider};
use strider_reader::ElfFileMemReader;

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
    let AnalyzeResult {
        cfg,
        function,
        unresolved_indirect_branches,
        isa_mode_conflicts,
        interior_branch_targets,
        unmapped_branch_targets,
        undecodable_branch_targets,
        unverified_seeded_sites,
    } = &result;
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
        "{regions:#?}\nunresolved {unresolved_indirect_branches:?}\n\
         unverified {unverified_seeded_sites:?}\nisa {isa_mode_conflicts:?}\n\
         interior {interior_branch_targets:?}\nunmapped {unmapped_branch_targets:?}\n\
         undecodable {undecodable_branch_targets:?}\n{}",
        function.to_text(true),
    )
}

#[test]
#[ignore = "broad fixture sweep; three analyses of every fixture function"]
fn a_reused_strider_analyses_every_fixture_function_as_a_fresh_one_does() {
    let mut total = 0usize;
    let mut failures = Vec::new();
    for arch in FIXTURE_ARCHES {
        for fixture in Fixture::all(arch) {
            let functions = fixture.functions();
            let (mut shared, cc) = fixture.strider();
            let fresh: Vec<String> = functions
                .iter()
                .map(|&(addr, _)| fingerprint(&mut fixture.strider().0, &cc, addr))
                .collect();
            for pass in 1..=2 {
                for (&(addr, _), want) in functions.iter().zip(&fresh) {
                    let got = fingerprint(&mut shared, &cc, addr);
                    if &got != want {
                        failures.push(format!(
                            "{} {addr:#x} pass {pass}:\nfresh:\n{want}\nshared:\n{got}",
                            fixture.path.display()
                        ));
                    }
                }
            }
            total += functions.len();
        }
        eprintln!("{}: done, {total} functions so far", arch.dir);
    }
    assert!(
        failures.is_empty(),
        "{} of {total} functions differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
